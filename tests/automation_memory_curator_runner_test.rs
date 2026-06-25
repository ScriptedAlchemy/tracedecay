mod automation_runner_support;

use automation_runner_support::*;

#[tokio::test]
async fn memory_curator_runner_skips_when_automation_is_disabled() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let backend = JsonBackend::new(json!({"ops": []}));

    let run = run_memory_curator_with_backend(
        &cg,
        &AutomationConfig::default(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("automation_disabled")
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
}

#[tokio::test]
async fn memory_curator_runner_validates_backend_ops_and_records_ledger() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [
            {
                "cluster_id": "cluster-0000",
                "op": "delete",
                "fact_id": 102,
                "confidence": 0.98,
                "reason": "near duplicate of fact 101"
            },
            {
                "cluster_id": "cluster-0000",
                "op": "delete",
                "fact_id": 999,
                "confidence": 0.98,
                "reason": "hallucinated id should be rejected"
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        model: Some("configured-model".to_string()),
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

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
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
    assert_eq!(run.ledger_record.rejected_count, 1);
    assert_eq!(run.ledger_record.reviewed_count, 2);
    assert_eq!(run.ledger_record.skipped_count, 0);
    assert_eq!(run.ledger_record.backend, "codex_app_server");
    assert_eq!(run.ledger_record.host_mode.as_deref(), Some("standalone"));
    assert_eq!(run.ledger_record.model.as_deref(), Some("fixture-model"));
    assert!(run
        .ledger_record
        .evidence_hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert!(run
        .ledger_record
        .input_hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert!(run
        .ledger_record
        .output_hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert_eq!(
        run.ledger_record.applied_ops.as_ref().unwrap()[0]["fact_id"],
        json!(102)
    );
    assert_eq!(
        run.ledger_record.rejected_ops.as_ref().unwrap()[0]["rejected_reason"],
        json!("fact_id 999 was not in reviewed evidence")
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["clusters_reviewed"],
        json!(1)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["apply_policy"]["decision"],
        json!("requires_dashboard_approval")
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["apply_policy"]
            ["permanent_delete_count"],
        json!(1)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["apply_policy"]["mutates_store"],
        json!(false)
    );
    assert_eq!(
        run.report["automation_apply_policy"]["approval_required"],
        json!(true)
    );
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
        json!("passed_with_rejections")
    );
    assert_eq!(validation_payload["loop_stage"], json!("validation_gate"));
    assert_eq!(
        validation_payload["improvement_gate"]["decision"],
        json!("ready_for_optimizer_review")
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
        validation_payload["improvement_gate"]["criteria"]["auto_apply_allowed"],
        json!(false)
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
        json!("ready_for_optimizer_review")
    );
    assert!(validation_payload["improvement_gate"]["artifact_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("generated_evals")
            && reference["sha256"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))));
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
    assert_eq!(feedback_payload["summary"]["rejected_count"], json!(1));
    assert_eq!(feedback_payload["summary"]["reviewed_count"], json!(2));
    assert_eq!(feedback_payload["human"], json!([]));
    assert!(feedback_payload["artifact_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("traces")
            && reference["sha256"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))));
    assert_eq!(feedback_payload["model"].as_array().unwrap().len(), 2);
    assert!(feedback_payload["model"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["outcome"] == json!("accepted")));
    assert!(feedback_payload["model"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["outcome"] == json!("rejected")
            && entry["reason"] == json!("fact_id 999 was not in reviewed evidence")));

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
    assert_eq!(eval_payload["promotion"]["auto_apply"], json!(false));
    assert_eq!(eval_payload["source_refs"][0]["kind"], json!("traces"));
    assert_eq!(eval_payload["source_refs"][1]["kind"], json!("feedback"));
    assert_eq!(
        eval_payload["eval_definitions"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        eval_payload["format"],
        json!("tracedecay_automation_eval:v1")
    );
    assert_eq!(eval_payload["runner"]["type"], json!("validation_replay"));
    assert_eq!(
        eval_payload["runner"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test memory_curator_runner_validates_backend_ops_and_records_ledger -- --nocapture"
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
        json!(2)
    );
    assert!(eval_payload["runner"]["inputs"]["validation_report_hash"]
        .as_str()
        .is_some_and(|hash| hash.starts_with("sha256:")));
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
    assert_eq!(eval_payload["promotion"]["state"], json!("validated"));
    assert_eq!(
        eval_payload["promotion"]["requires_human_review"],
        json!(true)
    );
    assert!(eval_payload["artifact_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("feedback")
            && reference["sha256"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))));
    assert!(eval_payload["eval_definitions"]
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
            && entry["assertions"][0]["type"] == json!("outcome_equals")));
    assert!(eval_payload["eval_definitions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["expected_outcome"] == json!("rejected")
            && entry["eval_id"] == json!("memory_curator:rejected:0")
            && entry["source_feedback_ref"] == json!("rejected:0")
            && entry["source_feedback"]["outcome"] == json!("rejected")
            && entry["expected"]["reason"] == json!("fact_id 999 was not in reviewed evidence")
            && entry["input"]["evidence_hash"] == json!(run.ledger_record.evidence_hash)
            && entry["input"]["input_hash"] == json!(run.ledger_record.input_hash)));
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
    assert_eq!(optimizer_payload["signals"]["rejected_count"], json!(1));
    assert_eq!(optimizer_payload["signals"]["reviewed_count"], json!(2));
    assert_eq!(
        optimizer_payload["signals"]["validation_gate_decision"],
        json!("ready_for_optimizer_review")
    );
    assert!(optimizer_payload["artifact_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("traces")));
    assert!(optimizer_payload["diagnostic_inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("generated_evals")
            && reference["sha256"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))));
    assert_eq!(optimizer_payload["blockers"], json!([]));
    assert_eq!(
        optimizer_payload["recommendations"][0]["id"],
        json!("review_rejections")
    );
    assert_eq!(
        optimizer_payload["ranked_changes"][0]["priority"],
        json!("high")
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
        json!("ready_for_optimizer_review")
    );
    assert_eq!(handoff_payload["readiness"]["eval_count"], json!(2));
    assert_eq!(
        handoff_payload["readiness"]["auto_apply_allowed"],
        json!(false)
    );
    assert_eq!(
        handoff_payload["machine_summary"]["next_stage"],
        json!("codex_review")
    );
    assert_eq!(
        handoff_payload["validation_requirements"]["must_not_auto_apply"],
        json!(true)
    );
    assert_eq!(
        handoff_payload["source_refs"][0]["kind"],
        json!("validation_gate")
    );
    assert!(handoff_payload["artifact_manifest"]["refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("optimizer_diagnosis")));
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
            "cargo test --test automation_runner_test memory_curator_runner_validates_backend_ops_and_records_ledger -- --nocapture"
        )
    );
    assert!(handoff_payload["request"]["evidence_hash"]
        .as_str()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert_eq!(run.report["llm_apply"]["ops"][0]["fact_id"], json!(102));
    assert_eq!(
        run.report["llm_apply"]["rejected_ops"][0]["rejected_reason"],
        json!("fact_id 999 was not in reviewed evidence")
    );

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
    assert_eq!(records[0].accepted_count, 1);
    assert_eq!(records[0].rejected_count, 1);
    assert_eq!(records[0].artifacts.len(), 6);
    assert!(
        fact_exists(&cg, 102).await,
        "dry-run memory curator must not delete accepted ops before approval"
    );
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

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.accepted_count, 0);
    assert_eq!(run.ledger_record.rejected_count, 0);
    assert_eq!(run.ledger_record.reviewed_count, 0);

    let eval_payload = read_artifact(&cg, &run.run_id, &run.ledger_record, "generated_evals").await;
    assert_eq!(eval_payload["summary"]["eval_count"], json!(0));
    assert_eq!(
        eval_payload["promotion"]["state"],
        json!("blocked_no_examples")
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
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [{
            "cluster_id": "cluster-0000",
            "op": "delete",
            "fact_id": 102,
            "confidence": 0.98,
            "reason": "near duplicate of fact 101"
        }]
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

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 0);

    let eval_payload = read_artifact(&cg, &run.run_id, &run.ledger_record, "generated_evals").await;
    assert_eq!(eval_payload["runner"]["status"], json!("passed"));
    assert_eq!(eval_payload["promotion"]["state"], json!("validated"));

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
        json!("codex_review")
    );
}

#[tokio::test]
async fn memory_curator_runner_auto_apply_is_blocked_by_dashboard_approval() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [{
            "cluster_id": "cluster-0000",
            "op": "delete",
            "fact_id": 102,
            "confidence": 0.98,
            "reason": "near duplicate of fact 101"
        }]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        auto_apply_memory_ops: true,
        require_dashboard_approval: true,
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

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(
        run.report["automation_apply_policy"]["decision"],
        json!("requires_dashboard_approval")
    );
    assert_eq!(
        run.report["automation_apply_policy"]["auto_apply_memory_ops"],
        json!(true)
    );
    assert_eq!(
        run.report["automation_apply_policy"]["mutates_store"],
        json!(false)
    );
    assert_eq!(run.report["llm_apply"]["applied"], Value::Null);
    assert!(
        fact_exists(&cg, 102).await,
        "dashboard approval must block permanent delete auto-apply"
    );
}

#[tokio::test]
async fn memory_curator_runner_auto_applies_only_when_approval_is_not_required() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [{
            "cluster_id": "cluster-0000",
            "op": "delete",
            "fact_id": 102,
            "confidence": 0.98,
            "reason": "near duplicate of fact 101"
        }]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        auto_apply_memory_ops: true,
        require_dashboard_approval: false,
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

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(
        run.report["automation_apply_policy"]["decision"],
        json!("auto_apply_allowed")
    );
    assert_eq!(
        run.report["automation_apply_policy"]["mutates_store"],
        json!(true)
    );
    assert_eq!(run.report["llm_apply"]["applied"], json!(1));
    assert_eq!(
        run.report["llm_apply"]["results"][0]["status"],
        json!("deleted")
    );
    assert!(
        !fact_exists(&cg, 102).await,
        "explicit no-approval auto-apply policy should delete accepted fact"
    );
}

#[tokio::test]
async fn memory_curator_runner_ledgers_malformed_backend_output() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = MalformedTextBackend::new(AgentTaskKind::MemoryCurator, "not json");
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

    let err = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
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
    assert_eq!(records[0].task, AgentTaskKind::MemoryCurator);
    assert_eq!(records[0].task_key.as_deref(), Some("memory_curator"));
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
async fn memory_curator_runner_records_noop_fallback_when_backend_run_task_fails() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = FailingBackend::new(AgentTaskKind::MemoryCurator);
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

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_noop_fallback_record(
        &run.ledger_record,
        AgentTaskKind::MemoryCurator,
        "memory_curator",
        json!({ "ops": [] }),
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
        AgentTaskKind::MemoryCurator,
        "memory_curator",
        json!({ "ops": [] }),
    );
}

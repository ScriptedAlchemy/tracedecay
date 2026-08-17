use crate::dashboard_api_support::*;
use tracedecay_agent_hosts::automation::backend::AgentTaskRetryAttempt;

#[test]
fn fact_store_curate_is_the_only_public_manual_automation_launcher() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let mut fixture = start_dashboard_configuration_fixture().await;
        let project_id = "dashboard_fixture_project";
        let agent = http_agent();
        let base_url = fixture.base_url.clone();

        let config_url = format!("{base_url}/api/plugins/holographic/curation/config");
        let (status, current_config) = get_json(&agent, &config_url);
        assert_eq!(status, 200, "config read should succeed: {current_config}");
        let expected_revision_id = current_config["configuration_revision_id"]
            .as_str()
            .expect("config read must return the pinned revision");
        let (status, saved_config) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({
                "expected_revision_id": expected_revision_id,
                "idempotency_key": "dashboard-automation-single-public-launcher",
                "enabled": false,
                "backend": "disabled"
            }),
        );
        assert_eq!(status, 200, "config patch should succeed: {saved_config}");
        assert_eq!(saved_config["effective"]["backend"], "disabled");

        let curate_url = format!("{base_url}/api/application/retained/fact_store_curate");
        let response = crate::common::http_call_with_retry("POST fact_store_curate", || {
            agent
                .post(&curate_url)
                .header(
                    tracedecay_application::APPLICATION_REQUEST_ID_HEADER,
                    "request.dashboard.fact-store-curate-single-launcher",
                )
                .send_json(&serde_json::json!({
                    "fact_review_limit": 24,
                    "min_confidence_millionths": 720_000
                }))
        });
        let status = response.status().as_u16();
        let mut response = response;
        let body = response
            .body_mut()
            .read_to_string()
            .expect("fact_store_curate response body");
        assert!(
            !body.is_empty(),
            "fact_store_curate returned status {status} without a body"
        );
        let memory_payload: serde_json::Value =
            serde_json::from_str(&body).expect("fact_store_curate JSON response");
        assert_eq!(status, 200);
        let memory_run = &memory_payload["value"]["outcome"]["value"]["payload"];
        assert_eq!(memory_run["terminal"]["status"], "skipped");
        assert_eq!(memory_run["task"], "memory_curator");
        assert_eq!(memory_run["terminal"]["reason"], "backend_disabled");

        let response = crate::common::http_call_with_retry(
            "POST fact_store_curate with caller-selected operations",
            || {
                agent
                    .post(&curate_url)
                    .header(
                        tracedecay_application::APPLICATION_REQUEST_ID_HEADER,
                        "request.dashboard.fact-store-curate-reject-operations",
                    )
                    .send_json(&serde_json::json!({
                        "fact_review_limit": 24,
                        "operations": []
                    }))
            },
        );
        let (status, rejected_manual_surface) = response_to_json(response);
        assert_eq!(status, 400);
        assert_eq!(rejected_manual_surface["kind"], "problem");

        let retired_curator_url = format!("{base_url}/api/automation/run/memory-curator");
        let response =
            crate::common::http_call_with_retry(&format!("POST {retired_curator_url}"), || {
                agent
                    .post(&retired_curator_url)
                    .send_json(&serde_json::json!({}))
            });
        let status = response.status().as_u16();
        assert_eq!(
            status, 404,
            "retired duplicate curator route must stay absent"
        );

        for route in ["session-reflection", "skill-writing"] {
            for url in [
                format!("{base_url}/api/automation/run/{route}"),
                format!("{base_url}/api/projects/{project_id}/automation/run/{route}"),
            ] {
                let response = crate::common::http_call_with_retry(&format!("POST {url}"), || {
                    agent.post(&url).send_json(&serde_json::json!({}))
                });
                assert_eq!(response.status().as_u16(), 404, "retired launcher: {url}");
            }
        }

        let (status, runs) = get_json(&agent, &format!("{base_url}/api/automation/runs?limit=5"));
        assert_eq!(status, 200);
        assert_eq!(runs["count"], 1);
        assert_eq!(runs["limit"], 5);
        assert_eq!(runs["runs"][0]["trigger"], "application");
        assert_eq!(runs["runs"][0]["status"], "skipped");
        assert_eq!(runs["runs"][0]["error"], "backend_disabled");

        let (status, runs) = get_json(&agent, &format!("{base_url}/api/automation/runs"));
        assert_eq!(status, 200);
        assert_eq!(runs["count"], 1);
        assert!(
            runs["runs"].as_array().is_some_and(|records| records
                .iter()
                .any(|record| record["run_id"] == memory_run["run_id"]
                    && record["status"] == "skipped")),
            "memory-curator run should remain visible in newest-first history: {runs}"
        );
        drop(agent);
        fixture.server.stop();
    });
}

#[test]
fn final_self_improvement_smoke_covers_autonomous_curation_and_skill_deployment() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let (cg, host_runtime) = setup_project(&project_root).await;
        let fixture = seed_memory_fixture(&cg).await;
        let curated_fact_id = fixture.near_duplicate_fact_id.clone();
        let fake_codex = FakeCodexAppServer::new_memory_curator(
            fixture.near_duplicate_fact_id.clone(),
            fixture.near_duplicate_last_event_id.clone(),
        );
        let _codex_bin_guard = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &fake_codex.bin);
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server_with_configuration_runtime(
            cg,
            host_runtime,
            dashboard::DashboardTestProjectGraphsV1::default(),
            port,
        );
        wait_for_dashboard(&agent, &base_url).await;

        let config_url = format!("{base_url}/api/plugins/holographic/curation/config");
        let (status, current_config) = get_json(&agent, &config_url);
        assert_eq!(status, 200, "config read should succeed: {current_config}");
        let expected_revision_id = current_config["configuration_revision_id"]
            .as_str()
            .unwrap_or_else(|| panic!("config read must return the pinned revision: {current_config}"));
        let (status, config) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({
                "expected_revision_id": expected_revision_id,
                "idempotency_key": "dashboard-final-self-improvement-smoke",
                "enabled": true,
                "backend": "codex_app_server",
                "host_mode": "standalone",
                "memory_curator": { "enabled": true, "schedule": "manual" }
            }),
        );
        assert_eq!(status, 200, "automation config patch failed: {config}");
        assert_eq!(config["effective"]["enabled"], true);
        assert_eq!(config["effective"]["backend"], "codex_app_server");

        let (status, completed) = post_json_body(
            &agent,
            &format!("{base_url}/api/application/retained/fact_store_curate"),
            &serde_json::json!({
                "fact_review_limit": 4,
                "min_confidence_millionths": 500_000
            }),
        );
        assert_eq!(
            status, 200,
            "dashboard automation run failed: {completed}"
        );
        let run = &completed["value"]["outcome"]["value"]["payload"];
        let run_id = run["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("completed response should include run_id: {completed}"))
            .to_string();
        assert_eq!(run["terminal"]["status"], "completed");
        assert_eq!(run["terminal"]["summary"]["accepted_count"], 1);
        assert_eq!(run["terminal"]["summary"]["rejected_count"], 0);
        let records = tracedecay_agent_hosts::automation::run_ledger::load_run_records(
            &dashboard_root,
            10,
        )
        .await
        .unwrap();
        let record = records
            .iter()
            .find(|record| record.run_id == run_id)
            .unwrap_or_else(|| panic!("returned run must be durably visible: {records:#?}"));
        let proposed = record
            .proposed_ops
            .as_ref()
            .and_then(|value| value["ops"].as_array())
            .unwrap_or_else(|| panic!("memory curator must retain proposed operations: {record:#?}"));
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0]["op"], "normalize_tags");
        assert_eq!(
            proposed[0]["target"]["fact_id"].as_str(),
            Some(curated_fact_id.as_str())
        );
        assert!(proposed[0]["target"]["fact_id"]
            .as_str()
            .is_some_and(|raw| FactId::new(raw.to_owned()).is_ok()));
        let applied = record
            .applied_ops
            .as_ref()
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("accepted curation needs an applied receipt: {record:#?}"));
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0]["op"], "curation_batch");
        assert_eq!(applied[0]["status"], "applied");
        assert_eq!(applied[0]["operation_count"], 1);
        assert_eq!(applied[0]["receipt"]["normalized_tags"], 1);
        assert_eq!(applied[0]["receipt"]["facts_linked"], 0);
        assert_eq!(
            applied[0]["receipt"]["changed_fact_ids"],
            serde_json::json!([curated_fact_id.as_str()])
        );
        assert_eq!(
            applied[0]["receipt"]["operation_effects"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert!(applied[0]["receipt"]["replay_event_id"].is_string());
        assert!(applied[0]["receipt"]["replay_fact_id"]
            .as_str()
            .is_some_and(|raw| FactId::new(raw.to_owned()).is_ok()));
        let (status, curated_fact) = get_json(
            &agent,
            &format!(
                "{base_url}/api/plugins/holographic/fact/{}",
                curated_fact_id.as_str()
            ),
        );
        assert_eq!(status, 200, "curated fact read failed: {curated_fact}");
        assert_eq!(curated_fact["domain_state"], "ready");
        assert_eq!(
            curated_fact["payload"]["fact"]["tags"],
            serde_json::json!(["cache", "curated", "policy"]),
            "automation must apply the reviewed canonical tag normalization"
        );

        let artifact_url = format!("{base_url}/api/automation/runs/{run_id}/artifacts");
        let (status, listed) = get_json(&agent, &artifact_url);
        assert_eq!(status, 200, "artifact list failed: {listed}");
        assert_eq!(listed["count"], 6);
        assert_eq!(listed["artifact_chain"]["complete"], true);
        assert_eq!(
            listed["artifact_chain"]["present_kinds"],
            serde_json::json!([
                "traces",
                "feedback",
                "generated_evals",
                "validation_gate",
                "optimizer_diagnosis",
                "codex_handoff"
            ])
        );

        let (status, evals) = get_json(&agent, &format!("{artifact_url}/generated_evals"));
        assert_eq!(status, 200, "generated eval artifact failed: {evals}");
        assert_eq!(evals["payload"]["format"], "tracedecay_automation_eval:v1");
        assert_eq!(evals["payload"]["runner"]["status"], "passed");
        assert_eq!(
            evals["payload"]["runner"]["results"][0]["status"],
            "passed"
        );
        assert_eq!(
            evals["payload"]["automatic_application"]["status"],
            "applied"
        );
        assert_eq!(
            evals["payload"]["eval_definitions"][0]["eval_id"],
            "memory_curator:accepted:0"
        );

        let (status, gate) = get_json(&agent, &format!("{artifact_url}/validation_gate"));
        assert_eq!(status, 200, "validation gate artifact failed: {gate}");
        assert_eq!(gate["payload"]["task_validation"]["decision"], "passed");
        assert_eq!(
            gate["payload"]["improvement_gate"]["decision"],
            "ready_for_handoff"
        );
        assert_eq!(
            gate["payload"]["improvement_gate"]["generated_evals_status"],
            "passed"
        );

        let (status, handoff) = get_json(&agent, &format!("{artifact_url}/codex_handoff"));
        assert_eq!(status, 200, "Codex handoff artifact failed: {handoff}");
        assert_eq!(handoff["payload"]["status"], "ready_for_review");
        assert_eq!(
            handoff["payload"]["machine_summary"]["next_stage"],
            "monitor_applied_outcomes"
        );
        assert_eq!(
            handoff["payload"]["artifact_manifest"]["api_list"],
            format!("/api/automation/runs/{run_id}/artifacts")
        );
        assert!(
            handoff["payload"]["artifact_manifest"]["refs"]
                .as_array()
                .is_some_and(|refs| refs
                    .iter()
                    .any(|reference| reference["kind"] == "optimizer_diagnosis")),
            "handoff should preserve upstream artifact refs: {handoff}"
        );

        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, created_skill) = post_json_body(
            &agent,
            &skills_url,
            &serde_json::json!({
                "id": "final-smoke-review",
                "title": "Final smoke review",
                "summary": "Inspect self-improvement run artifacts and active skill state.",
                "category": "workflow",
                "body_markdown": "Check the run ledger, generated evals, validation gate, and active skill.",
                "targets": ["codex"],
                "provenance": {
                    "source": "automation_run",
                    "actor": "dashboard-smoke",
                    "run_id": run_id
                }
            }),
        );
        assert_eq!(status, 200, "skill create should be accepted: {created_skill}");
        assert_eq!(created_skill["skill"]["metadata"]["state"], "active");
        assert_eq!(
            created_skill["skill"]["metadata"]["provenance"]["run_id"],
            run_id
        );

        let (status, skill_detail) = get_json(
            &agent,
            &format!("{base_url}/api/automation/skills/final-smoke-review"),
        );
        assert_eq!(status, 200, "active skill should remain inspectable: {skill_detail}");
        assert_eq!(skill_detail["skill"]["metadata"]["state"], "active");
        assert_eq!(
            skill_detail["skill"]["metadata"]["provenance"]["source"],
            "automation_run"
        );

        let (status, runs) = get_json(
            &agent,
            &format!("{base_url}/api/automation/runs?limit=5"),
        );
        assert_eq!(status, 200);
        assert!(
            runs["runs"]
                .as_array()
                .is_some_and(
                    |records| records.iter().any(|record| record["run_id"] == run_id
                        && record["status"] == "succeeded"
                        && record["artifact_kinds"]
                            .as_array()
                            .is_some_and(|artifacts| artifacts.len() == 6))
                ),
            "successful dashboard automation run should be visible in history: {runs}"
        );

        server.stop();
    });
}

#[test]
fn automation_run_artifact_api_serves_verified_sidecar_payloads() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let (cg, host_runtime) = setup_project(&project_root).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let run_id = "artifact_api_run";
        let created_at = "2026-06-24T00:00:00Z";
        // Schema-v2 ledger rows carry nonnegative Unix seconds; the RFC3339
        // form above stays for the schema-v1 artifact sidecar only.
        let ledger_unix_seconds = "1782259200";
        let artifact = tracedecay_agent_hosts::automation::run_ledger::write_run_artifact(
            &dashboard_root,
            run_id,
            tracedecay_agent_hosts::automation::run_ledger::AutomationRunArtifactKind::CodexHandoff,
            &serde_json::json!({
                "schema_version": 1,
                "run_id": run_id,
                "status": "ready_for_review",
                "next_actions": ["review dashboard artifact payload"]
            }),
            Some("handoff ready".to_string()),
            created_at,
        )
        .await
        .unwrap();
        tracedecay_agent_hosts::automation::run_ledger::append_run_record(
            &dashboard_root,
            &tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord {
                schema_version: 2,
                run_id: run_id.to_string(),
                trigger:
                    tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger::ManualCli,
                task: tracedecay_agent_hosts::automation::backend::AgentTaskKind::MemoryCurator,
                task_key: Some("memory_curator".to_string()),
                backend: "codex_app_server".to_string(),
                host_mode: Some("standalone".to_string()),
                prompt_version: Some("memory_curator:v1".to_string()),
                response_schema: None,
                strict_json: None,
                model: Some("test-model".to_string()),
                status:
                    tracedecay_agent_hosts::automation::run_ledger::AutomationRunStatus::Succeeded,
                evidence_hash: Some("sha256:evidence".to_string()),
                input_hash: Some("sha256:input".to_string()),
                output_hash: Some("sha256:output".to_string()),
                proposed_ops: None,
                applied_ops: None,
                rejected_ops: None,
                validation_report: None,
                reviewed_count: 1,
                accepted_count: 1,
                rejected_count: 0,
                skipped_count: 0,
                error: None,
                error_classification: None,
                error_retryable: None,
                backend_attempt_count: 1,
                backend_attempts: vec![AgentTaskRetryAttempt {
                    attempt: 1,
                    succeeded: true,
                    failure_classification: None,
                    backoff_millis: 0,
                }],
                fallback_status: None,
                report_ref: None,
                artifacts: vec![artifact],
                started_at: ledger_unix_seconds.to_string(),
                completed_at: ledger_unix_seconds.to_string(),
                completed_at_micros: Some(1_782_259_200_000_000),
            },
        )
        .await
        .unwrap();

        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server_with_host_runtime(
            cg,
            host_runtime,
            dashboard::DashboardTestProjectGraphsV1::default(),
            port,
        );
        wait_for_dashboard(&agent, &base_url).await;

        let artifact_url = format!("{base_url}/api/automation/runs/{run_id}/artifacts");
        let (status, listed) = get_json(&agent, &artifact_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["artifacts"][0]["kind"], "codex_handoff");
        assert_eq!(listed["artifacts"][0]["summary"], "handoff ready");
        assert_eq!(listed["artifact_chain"]["complete"], false);
        assert_eq!(
            listed["artifact_chain"]["expected_kinds"],
            serde_json::json!([
                "traces",
                "feedback",
                "generated_evals",
                "validation_gate",
                "optimizer_diagnosis",
                "codex_handoff"
            ])
        );
        assert_eq!(
            listed["artifact_chain"]["present_kinds"],
            serde_json::json!(["codex_handoff"])
        );

        let (status, payload) = get_json(&agent, &format!("{artifact_url}/codex_handoff"));
        assert_eq!(status, 200);
        assert_eq!(payload["artifact"]["kind"], "codex_handoff");
        assert_eq!(payload["payload"]["run_id"], run_id);
        assert_eq!(payload["payload"]["status"], "ready_for_review");

        let (status, missing) = get_json(&agent, &format!("{artifact_url}/validation_gate"));
        assert_eq!(status, 404);
        assert!(
            missing["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("not found"))
        );

        let artifact_path = tracedecay_agent_hosts::automation::run_ledger::run_artifact_path(
            &dashboard_root,
            run_id,
            tracedecay_agent_hosts::automation::run_ledger::AutomationRunArtifactKind::CodexHandoff,
        )
        .unwrap();
        std::fs::write(&artifact_path, "{\"tampered\":true}\n").unwrap();
        let (status, tampered) = get_json(&agent, &format!("{artifact_url}/codex_handoff"));
        assert_eq!(status, 500);
        assert!(
            tampered["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("hash mismatch"))
        );

        server.stop();
    });
}

#[test]
fn automation_outcomes_endpoint_reports_activated_skills_and_automatic_fact_receipt_trajectories() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let (cg, host_runtime) = setup_project(&project_root).await;
        use tracedecay_agent_hosts::automation::managed_skills::{
            ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, create_managed_skill,
            default_managed_skill_targets,
        };

        let managed_skill_profile_root = host_runtime.profile_root().to_path_buf();
        create_managed_skill(
            &managed_skill_profile_root,
            ManagedSkillDraft {
                id: "dashboard-outcome-trajectory-skill".to_owned(),
                title: "Dashboard outcome trajectory skill".to_owned(),
                summary: "Fixture for automatic outcome tracking.".to_owned(),
                category: "maintenance".to_owned(),
                targets: default_managed_skill_targets(),
                body_markdown: "Use when checking automatic outcome trajectories.".to_owned(),
                support_files: Vec::new(),
                provenance: ManagedSkillProvenance {
                    source: ManagedSkillSource::AutomationRun,
                    actor: "tracedecay".to_owned(),
                    run_id: Some("run_outcomes_skill".to_owned()),
                },
            },
        )
        .await
        .unwrap_or_else(|error| panic!("create active managed skill: {error}"));

        let alive_receipt = record_dashboard_automatic_fact(
            &cg,
            "run_outcomes_alive",
            "Automation outcomes retain canonical applied fact identity",
        )
        .await;
        let gone_receipt = record_dashboard_automatic_fact(
            &cg,
            "run_outcomes_deleted",
            "Automation outcomes retain deleted fact lineage safely",
        )
        .await;
        delete_dashboard_automatic_fact(&cg, &gone_receipt).await;

        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server_with_host_runtime(
            cg,
            host_runtime,
            dashboard::DashboardTestProjectGraphsV1::default(),
            port,
        );
        wait_for_dashboard(&agent, &base_url).await;

        let (status, outcomes) = get_json(&agent, &format!("{base_url}/api/automation/outcomes"));
        assert_eq!(status, 200, "outcomes endpoint failed: {outcomes}");
        assert_eq!(outcomes["error"], "");
        let skills = outcomes["skills"]
            .as_array()
            .unwrap_or_else(|| panic!("skills must be an array: {outcomes}"));
        let skill = skills
            .iter()
            .find(|skill| skill["skill_id"] == "dashboard-outcome-trajectory-skill")
            .unwrap_or_else(|| panic!("missing activated skill outcome: {outcomes}"));
        assert_eq!(skill["verdict"], "too_early");
        assert!(skill["activated_at"].is_number());
        assert!(skill["days_since_activation"].is_number());

        let facts = outcomes["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("facts must be an array: {outcomes}"));
        assert_eq!(facts.len(), 2);
        let by_id = |id: &str| {
            facts
                .iter()
                .find(|fact| fact["apply_id"] == id)
                .unwrap_or_else(|| panic!("missing automatic fact receipt {id}: {outcomes}"))
        };
        let alive = by_id(&alive_receipt.apply_id);
        assert_eq!(alive["verdict"], "never_recalled");
        assert_eq!(alive["still_exists"], true);
        assert_eq!(alive["helpful_count"], 0);
        assert_eq!(
            alive["canonical_fact_id"],
            serde_json::json!(alive_receipt.fact_id)
        );
        assert_eq!(alive["run_id"], "run_outcomes_alive");
        let gone = by_id(&gone_receipt.apply_id);
        assert_eq!(gone["verdict"], "deleted");
        assert_eq!(gone["still_exists"], false);
        assert_eq!(
            gone["canonical_fact_id"],
            serde_json::json!(gone_receipt.fact_id)
        );
        assert_eq!(gone["run_id"], "run_outcomes_deleted");

        server.stop();
    });
}

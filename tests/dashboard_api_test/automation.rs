use crate::dashboard_api_support::*;

#[test]
fn curation_agent_plan_skips_when_automation_is_disabled_and_records_history() {
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

        let cg = setup_project(&project_root).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let config_url = format!("{base_url}/api/plugins/holographic/curation/config");
        let (status, saved_config) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({
                "enabled": false,
                "backend": "codex_app_server",
                "host_mode": "delegated_host",
                "model": "queued-model"
            }),
        );
        assert_eq!(status, 200, "config patch should succeed: {saved_config}");
        assert_eq!(saved_config["effective"]["backend"], "codex_app_server");
        assert_eq!(saved_config["effective"]["host_mode"], "delegated_host");
        assert_eq!(saved_config["effective"]["model"], "queued-model");

        let (status, payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/agent-plan"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(payload["status"], "skipped");
        assert_eq!(payload["ledger_record"]["trigger"], "dashboard");
        assert_eq!(payload["ledger_record"]["error"], "automation_disabled");
        assert_eq!(payload["report"]["reason"], "automation_disabled");

        let (status, memory_payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/memory-curator"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 202);
        assert_eq!(memory_payload["status"], "queued");
        assert_eq!(memory_payload["ledger_record"]["trigger"], "dashboard");
        assert_eq!(memory_payload["ledger_record"]["task"], "memory_curator");
        assert_eq!(
            memory_payload["ledger_record"]["backend"],
            "codex_app_server"
        );
        assert_eq!(
            memory_payload["ledger_record"]["host_mode"],
            "delegated_host"
        );
        assert_eq!(memory_payload["ledger_record"]["model"], "queued-model");

        let (status, session_payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/session-reflection"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 202);
        assert_eq!(session_payload["status"], "queued");
        assert_eq!(session_payload["ledger_record"]["trigger"], "dashboard");
        assert_eq!(
            session_payload["ledger_record"]["task"],
            "session_reflector"
        );
        assert_eq!(
            session_payload["ledger_record"]["backend"],
            "codex_app_server"
        );
        assert_eq!(
            session_payload["ledger_record"]["host_mode"],
            "delegated_host"
        );
        assert_eq!(session_payload["ledger_record"]["model"], "queued-model");

        let (status, skill_payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/skill-writing"),
            &serde_json::json!({
                "dry_run": true,
                "provider": "cursor",
                "query": "workflow corrections",
                "evidence_limit": 7,
                "storage_scope": "project_local"
            }),
        );
        assert_eq!(status, 202);
        assert_eq!(skill_payload["status"], "queued");
        assert_eq!(skill_payload["ledger_record"]["trigger"], "dashboard");
        assert_eq!(skill_payload["ledger_record"]["task"], "skill_writer");
        assert_eq!(
            skill_payload["ledger_record"]["backend"],
            "codex_app_server"
        );
        assert_eq!(
            skill_payload["ledger_record"]["host_mode"],
            "delegated_host"
        );
        assert_eq!(skill_payload["ledger_record"]["model"], "queued-model");

        let mut rejected_skill_shape = agent
            .post(&format!("{base_url}/api/automation/run/skill-writing"))
            .send_json(serde_json::json!({
                "dry_run": true,
                "unsupported_field": true
            }))
            .expect("skill-writing request with unsupported field should receive response");
        let rejected_skill_status = rejected_skill_shape.status().as_u16();
        let rejected_skill_body = rejected_skill_shape
            .body_mut()
            .read_to_string()
            .expect("skill-writing rejection body should be readable");
        assert_eq!(rejected_skill_status, 422);
        assert!(
            rejected_skill_body.contains("unsupported_field"),
            "rejection should name the unsupported field: {rejected_skill_body}"
        );

        let (status, rejected) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/session-reflection"),
            &serde_json::json!({ "dry_run": false }),
        );
        assert_eq!(status, 400);
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("dry_run=true")),
            "dry-run guard should explain the approval-only contract: {rejected}"
        );

        let run_ids = [
            memory_payload["run_id"].as_str().unwrap().to_string(),
            session_payload["run_id"].as_str().unwrap().to_string(),
            skill_payload["run_id"].as_str().unwrap().to_string(),
        ];
        let mut records = Vec::new();
        let mut terminal_count = 0;
        for _ in 0..200 {
            records = tracedecay::automation::run_ledger::load_run_records(&dashboard_root, 10)
                .await
                .unwrap();
            terminal_count = records
                .iter()
                .filter(|record| {
                    run_ids.contains(&record.run_id)
                        && record.status.is_terminal()
                        && record.error.as_deref() == Some("automation_disabled")
                })
                .count();
            if terminal_count == run_ids.len() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(
            terminal_count,
            run_ids.len(),
            "dashboard automation jobs did not reach terminal skipped records: {records:#?}"
        );
        assert_eq!(records.len(), 4);
        let tasks: Vec<_> = records.iter().map(|record| record.task).collect();
        assert_eq!(
            tasks,
            [
                tracedecay::automation::backend::AgentTaskKind::SkillWriter,
                tracedecay::automation::backend::AgentTaskKind::SessionReflector,
                tracedecay::automation::backend::AgentTaskKind::MemoryCurator,
                tracedecay::automation::backend::AgentTaskKind::MemoryCurator,
            ]
        );
        for record in &records {
            assert_eq!(
                record.trigger,
                tracedecay::automation::run_ledger::AutomationTrigger::Dashboard
            );
            assert_eq!(
                record.status,
                tracedecay::automation::run_ledger::AutomationRunStatus::Skipped
            );
            assert_eq!(record.error.as_deref(), Some("automation_disabled"));
            assert_eq!(record.backend, "codex_app_server");
            assert_eq!(record.host_mode.as_deref(), Some("delegated_host"));
            assert_eq!(record.model.as_deref(), Some("queued-model"));
        }

        let (status, runs) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/runs?limit=5"),
        );
        assert_eq!(status, 200);
        assert_eq!(runs["count"], 4);
        assert_eq!(runs["limit"], 5);
        assert_eq!(runs["records"][0]["trigger"], "dashboard");
        assert_eq!(runs["records"][0]["status"], "skipped");
        assert_eq!(runs["records"][0]["error"], "automation_disabled");

        let (status, activity) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/activity"),
        );
        assert_eq!(status, 200);
        let events = activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected activity events array: {activity}"));
        let phases: Vec<_> = events
            .iter()
            .filter_map(|event| event["phase"].as_str())
            .collect();
        for phase in [
            "queued",
            "evidence",
            "backend",
            "validation",
            "apply",
            "report",
            "finish",
        ] {
            assert!(
                phases.contains(&phase),
                "agent-plan should emit {phase} activity; phases={phases:?}, activity={activity}"
            );
        }
        let memory_skip_phases: Vec<_> = events
            .iter()
            .filter(|event| {
                event["message"].as_str().is_some_and(|message| {
                    message
                        .to_ascii_lowercase()
                        .contains("dashboard memory-curator automation run")
                })
            })
            .filter_map(|event| event["phase"].as_str())
            .collect();
        for phase in [
            "queued",
            "evidence",
            "backend",
            "validation",
            "apply",
            "report",
            "finish",
        ] {
            assert!(
                memory_skip_phases.contains(&phase),
                "queued memory-curator skip should emit {phase} activity; phases={memory_skip_phases:?}, activity={activity}"
            );
        }
        for task_label in ["session-reflector", "skill-writer"] {
            let task_skip_phases: Vec<_> = events
                .iter()
                .filter(|event| {
                    event["message"].as_str().is_some_and(|message| {
                        message
                            .to_ascii_lowercase()
                            .contains(&format!("dashboard {task_label} automation run"))
                    })
                })
                .filter_map(|event| event["phase"].as_str())
                .collect();
            for phase in [
                "queued",
                "evidence",
                "backend",
                "validation",
                "apply",
                "report",
                "finish",
            ] {
                assert!(
                    task_skip_phases.contains(&phase),
                    "queued {task_label} skip should emit {phase} activity; phases={task_skip_phases:?}, activity={activity}"
                );
            }
        }
        assert!(
            events.iter().any(|event| event["message"]
                .as_str()
                .is_some_and(|message| message
                    .contains("Dashboard memory-curator automation run skipped"))),
            "dashboard memory-curator queued skip should emit visible activity: {activity}"
        );
        assert!(
            events.iter().any(|event| event["phase"] == "report"),
            "agent-plan should write a visible curation activity event: {activity}"
        );
        assert!(
            events.iter().any(|event| {
                event["phase"] == "finish"
                    && event["dry_run"] == true
                    && event["message"].as_str().is_some_and(|message| {
                        message.contains("Finished standalone memory-curator agent plan")
                    })
            }),
            "agent-plan should emit a terminal finish activity event: {activity}"
        );

        let (status, runs) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/runs"),
        );
        assert_eq!(status, 200);
        assert_eq!(runs["count"], 4);
        assert!(
            runs["records"].as_array().is_some_and(|records| records
                .iter()
                .any(|record| record["run_id"] == memory_payload["run_id"]
                    && record["status"] == "skipped")),
            "memory-curator run should remain visible in newest-first history: {runs}"
        );
        server.stop();
    });
}

#[test]
fn dashboard_session_and_skill_runs_emit_activity_when_evidence_is_unavailable() {
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

        let cg = setup_project(&project_root).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let (status, config) = patch_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/config"),
            &serde_json::json!({
                "enabled": true,
                "backend": "codex_app_server",
                "host_mode": "standalone",
                "session_reflector": { "enabled": true, "schedule": "manual" },
                "skill_writer": { "enabled": true, "schedule": "manual" }
            }),
        );
        assert_eq!(status, 200, "automation config patch failed: {config}");

        let (status, session_payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/session-reflection"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 202, "session run should queue: {session_payload}");
        let session_run_id = session_payload["run_id"].as_str().unwrap().to_string();
        let mut records = Vec::new();

        let (status, skill_payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/skill-writing"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 202, "skill run should queue: {skill_payload}");
        let skill_run_id = skill_payload["run_id"].as_str().unwrap().to_string();

        let run_ids = [session_run_id, skill_run_id];
        let mut terminal_count = 0;
        for _ in 0..400 {
            records = tracedecay::automation::run_ledger::load_run_records(&dashboard_root, 10)
                .await
                .unwrap();
            terminal_count = records
                .iter()
                .filter(|record| run_ids.contains(&record.run_id) && record.status.is_terminal())
                .count();
            if terminal_count == run_ids.len() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(
            terminal_count,
            run_ids.len(),
            "dashboard automation jobs did not reach terminal records: {records:#?}"
        );
        for run_id in &run_ids {
            let terminal = records
                .iter()
                .find(|record| record.run_id == *run_id && record.status.is_terminal())
                .unwrap_or_else(|| panic!("missing terminal record for {run_id}: {records:#?}"));
            assert_eq!(
                terminal.status,
                tracedecay::automation::run_ledger::AutomationRunStatus::Skipped
            );
            assert!(
                terminal.error.as_deref().is_some_and(|reason| reason
                    == "lcm_not_ingested"
                    || reason == "no_session_evidence"
                    || reason == "no_skill_writer_evidence"),
                "unexpected evidence skip reason: {terminal:#?}"
            );
        }

        let (status, activity) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/activity?limit=50"),
        );
        assert_eq!(status, 200);
        let events = activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected activity events array: {activity}"));
        for task_label in ["session-reflector", "skill-writer"] {
            let task_phases: Vec<_> = events
                .iter()
                .filter(|event| {
                    event["message"].as_str().is_some_and(|message| {
                        message
                            .to_ascii_lowercase()
                            .contains(&format!("dashboard {task_label} automation run"))
                    })
                })
                .filter_map(|event| event["phase"].as_str())
                .collect();
            for phase in [
                "queued",
                "evidence",
                "backend",
                "validation",
                "apply",
                "report",
                "finish",
            ] {
                assert!(
                    task_phases.contains(&phase),
                    "queued {task_label} run should emit {phase} activity; phases={task_phases:?}, activity={activity}"
                );
            }
        }

        server.stop();
    });
}

#[test]
fn final_self_improvement_smoke_covers_manual_curation_skill_approval_and_dashboard_review() {
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
        let fake_codex = FakeCodexAppServer::new_memory_curator();
        let _codex_bin_guard = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &fake_codex.bin);

        let cg = setup_project(&project_root).await;
        seed_memory_fixture(&cg).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let (status, config) = patch_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/config"),
            &serde_json::json!({
                "enabled": true,
                "backend": "codex_app_server",
                "host_mode": "standalone",
                "model": "dashboard-configured-model",
                "memory_curator": { "enabled": true, "schedule": "manual" }
            }),
        );
        assert_eq!(status, 200, "automation config patch failed: {config}");
        assert_eq!(config["effective"]["enabled"], true);
        assert_eq!(config["effective"]["backend"], "codex_app_server");

        let (status, queued) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/memory-curator"),
            &serde_json::json!({
                "dry_run": true,
                "max_clusters": 4,
                "min_confidence": 0.5
            }),
        );
        assert_eq!(status, 202, "dashboard automation run failed: {queued}");
        assert_eq!(queued["status"], "queued");
        let run_id = queued["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("queued response should include run_id: {queued}"))
            .to_string();

        let mut record = None;
        for _ in 0..200 {
            let records = tracedecay::automation::run_ledger::load_run_records(&dashboard_root, 10)
                .await
                .unwrap();
            record = records
                .into_iter()
                .find(|record| record.run_id == run_id && record.status.is_terminal());
            if record.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let record = record.unwrap_or_else(|| {
            panic!("dashboard automation run did not reach a terminal ledger record")
        });
        assert_eq!(
            record.status,
            tracedecay::automation::run_ledger::AutomationRunStatus::Succeeded
        );
        assert_eq!(record.accepted_count, 1);
        assert_eq!(record.rejected_count, 0);
        assert_eq!(record.artifacts.len(), 6);

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
        assert_eq!(evals["payload"]["promotion"]["state"], "validated");
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
            "codex_review"
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
                "summary": "Review self-improvement run artifacts and approval state.",
                "category": "workflow",
                "body_markdown": "Check the run ledger, generated evals, validation gate, and pending skill approval before applying changes.",
                "targets": ["codex"],
                "provenance": {
                    "source": "automation_run",
                    "actor": "dashboard-smoke",
                    "run_id": run_id
                }
            }),
        );
        assert_eq!(status, 200, "skill draft should be accepted: {created_skill}");
        assert_eq!(
            created_skill["skill"]["metadata"]["state"],
            "pending_approval"
        );
        assert_eq!(
            created_skill["skill"]["metadata"]["provenance"]["run_id"],
            run_id
        );

        let (status, approved_skill) = post_json(
            &agent,
            &format!("{base_url}/api/automation/skills/final-smoke-review/approve"),
        );
        assert_eq!(status, 200, "skill approval should succeed: {approved_skill}");
        assert_eq!(approved_skill["skill"]["metadata"]["state"], "active");

        let (status, skill_detail) = get_json(
            &agent,
            &format!("{base_url}/api/automation/skills/final-smoke-review"),
        );
        assert_eq!(status, 200, "approved skill should remain reviewable: {skill_detail}");
        assert_eq!(skill_detail["skill"]["metadata"]["state"], "active");
        assert_eq!(
            skill_detail["skill"]["metadata"]["provenance"]["source"],
            "automation_run"
        );

        let (status, runs) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/runs?limit=5"),
        );
        assert_eq!(status, 200);
        assert!(
            runs["records"]
                .as_array()
                .is_some_and(
                    |records| records.iter().any(|record| record["run_id"] == run_id
                        && record["status"] == "succeeded"
                        && record["artifacts"]
                            .as_array()
                            .is_some_and(|artifacts| artifacts.len() == 6))
                ),
            "successful dashboard automation run should be visible in history: {runs}"
        );

        let (status, activity) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/activity?limit=20"),
        );
        assert_eq!(status, 200);
        let activity_events = activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected curation activity events: {activity}"));
        let activity_phases: Vec<_> = activity_events
            .iter()
            .filter_map(|event| event["phase"].as_str())
            .collect();
        for phase in [
            "queued",
            "evidence",
            "backend",
            "validation",
            "apply",
            "report",
            "finish",
        ] {
            assert!(
                activity_phases.contains(&phase),
                "successful dashboard automation run should emit {phase} activity; phases={activity_phases:?}, activity={activity}"
            );
        }

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

        let cg = setup_project(&project_root).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let run_id = "artifact_api_run";
        let created_at = "2026-06-24T00:00:00Z";
        let artifact = tracedecay::automation::run_ledger::write_run_artifact(
            &dashboard_root,
            run_id,
            tracedecay::automation::run_ledger::AutomationRunArtifactKind::CodexHandoff,
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
        tracedecay::automation::run_ledger::append_run_record(
            &dashboard_root,
            &tracedecay::automation::run_ledger::AutomationRunLedgerRecord {
                schema_version: 2,
                run_id: run_id.to_string(),
                trigger: tracedecay::automation::run_ledger::AutomationTrigger::ManualCli,
                task: tracedecay::automation::backend::AgentTaskKind::MemoryCurator,
                task_key: Some("memory_curator".to_string()),
                backend: "codex_app_server".to_string(),
                host_mode: Some("standalone".to_string()),
                prompt_version: Some("memory_curator:v1".to_string()),
                response_schema: None,
                strict_json: None,
                model: Some("test-model".to_string()),
                status: tracedecay::automation::run_ledger::AutomationRunStatus::Succeeded,
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
                fallback_status: None,
                report_ref: None,
                artifacts: vec![artifact],
                started_at: created_at.to_string(),
                completed_at: created_at.to_string(),
            },
        )
        .await
        .unwrap();

        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
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
        assert!(missing["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("not found")));

        let artifact_path = tracedecay::automation::run_ledger::run_artifact_path(
            &dashboard_root,
            run_id,
            tracedecay::automation::run_ledger::AutomationRunArtifactKind::CodexHandoff,
        )
        .unwrap();
        std::fs::write(&artifact_path, "{\"tampered\":true}\n").unwrap();
        let (status, tampered) = get_json(&agent, &format!("{artifact_url}/codex_handoff"));
        assert_eq!(status, 500);
        assert!(tampered["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("hash mismatch")));

        server.stop();
    });
}

#[test]
fn automation_outcomes_endpoint_reports_applied_fact_trajectories() {
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

        let cg = setup_project(&project_root).await;
        seed_memory_fixture(&cg).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();

        // One applied proposal whose fact still exists (seeded fact 101) and
        // one whose fact was deleted by curation (no such fact id).
        let applied = |proposal_id: &str, fact_id: i64| {
            tracedecay::automation::fact_proposals::FactProposalRecord {
                schema_version: 1,
                proposal_id: proposal_id.to_string(),
                run_id: "run_outcomes".to_string(),
                evidence_hash: None,
                state: tracedecay::automation::fact_proposals::FactProposalState::Applied,
                add_fact_request: None,
                proposal: None,
                validation_reason: None,
                validation: None,
                reviewer: Some("dashboard".to_string()),
                applied_fact_id: Some(fact_id),
                apply_outcome: None,
                created_at: 1_700_000_000,
                updated_at: 1_700_000_050,
            }
        };
        tracedecay::automation::fact_proposals::save_fact_proposal_store(
            &dashboard_root,
            &tracedecay::automation::fact_proposals::FactProposalStore {
                schema_version: 1,
                proposals: vec![applied("fact_alive", 101), applied("fact_gone", 999_999)],
            },
        )
        .await
        .unwrap();

        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let (status, outcomes) = get_json(&agent, &format!("{base_url}/api/automation/outcomes"));
        assert_eq!(status, 200, "outcomes endpoint failed: {outcomes}");
        assert_eq!(outcomes["error"], "");
        assert_eq!(outcomes["skills"], serde_json::json!([]));
        let facts = outcomes["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("facts must be an array: {outcomes}"));
        assert_eq!(facts.len(), 2);
        let by_id = |id: &str| {
            facts
                .iter()
                .find(|fact| fact["proposal_id"] == id)
                .unwrap_or_else(|| panic!("missing proposal {id}: {outcomes}"))
        };
        let alive = by_id("fact_alive");
        assert_eq!(alive["verdict"], "never_recalled");
        assert_eq!(alive["still_exists"], true);
        assert_eq!(alive["helpful_count"], 5);
        let gone = by_id("fact_gone");
        assert_eq!(gone["verdict"], "deleted");
        assert_eq!(gone["still_exists"], false);

        server.stop();
    });
}

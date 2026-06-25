mod common;
mod dashboard_api_support;

use dashboard_api_support::*;

#[test]
fn automation_config_is_dashboard_controllable_and_persistent() {
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
        let missing_codex_bin = tmp_root.join("missing-codex");
        let _codex_bin_guard = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &missing_codex_bin);

        let mut global_config = tracedecay::user_config::UserConfig::default();
        global_config.automation.enabled = true;
        global_config.automation.backend =
            tracedecay::automation::config::AutomationBackend::CodexAppServer;
        global_config.automation.model = Some("global-model".to_string());
        assert!(global_config.save(), "global user config should save");

        let cg = setup_project(&project_root).await;
        let sidecar = cg
            .store_layout()
            .dashboard_root
            .join("automation_config.json");
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let config_url = format!("{base_url}/api/plugins/holographic/curation/config");
        let (status, config) = get_json(&agent, &config_url);
        assert_eq!(status, 200);
        assert_eq!(config["global"]["enabled"], true);
        assert_eq!(config["global"]["backend"], "codex_app_server");
        assert_eq!(config["global"]["model"], "global-model");
        assert!(config["project"].is_null());
        assert_eq!(config["effective"]["model"], "global-model");
        assert_eq!(config["backend_availability"]["available"], false);
        assert_eq!(
            config["backend_availability"]["executable"],
            missing_codex_bin.display().to_string()
        );
        assert_eq!(
            config["effective"]["tasks"]["memory_curator"]["enabled"],
            false
        );

        let patch = serde_json::json!({
            "model": "project-model",
            "timeout_secs": 90,
            "scheduler_tick_secs": 15,
            "memory_curator": { "enabled": true, "schedule": "manual" }
        });
        let (status, saved) = patch_json_body(&agent, &config_url, &patch);
        assert_eq!(status, 200);
        assert_eq!(saved["project"]["model"], "project-model");
        assert_eq!(saved["effective"]["model"], "project-model");
        assert_eq!(saved["effective"]["timeout_secs"], 90);
        assert_eq!(saved["effective"]["scheduler_tick_secs"], 15);
        assert_eq!(
            saved["effective"]["tasks"]["memory_curator"]["schedule"],
            "manual"
        );
        assert!(sidecar.exists(), "PATCH must persist a project sidecar");

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["features"]["automation"], true);
        assert_eq!(capabilities["features"]["llm_curation"], true);
        assert_eq!(capabilities["automation"]["mode"], "standalone_backend");
        assert_eq!(capabilities["automation"]["backend"], "codex_app_server");
        assert_eq!(capabilities["automation"]["host_mode"], "standalone");
        assert_eq!(
            capabilities["automation"]["availability"]["available"],
            false
        );
        assert_eq!(
            capabilities["automation"]["availability"]["executable"],
            missing_codex_bin.display().to_string()
        );
        assert!(
            capabilities["automation"]["availability"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("was not found")),
            "capabilities should explain unavailable app-server backend: {capabilities}"
        );

        let scheduler_url = format!("{base_url}/api/automation/scheduler/status");
        let (status, scheduler) = get_json(&agent, &scheduler_url);
        assert_eq!(status, 200);
        assert_eq!(scheduler["status"], "configured");
        assert_eq!(scheduler["paused"], false);
        assert_eq!(scheduler["scheduler_tick_secs"], 15);
        assert!(
            scheduler["tasks"]
                .as_array()
                .is_some_and(|tasks| tasks.iter().any(|task| {
                    task["task"] == "memory_curator"
                        && task["due"] == false
                        && task["skip_reason"] == "scheduler_schedule_manual"
                })),
            "manual memory curator should be visible as a skipped scheduler task: {scheduler}"
        );

        let (status, paused) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/scheduler/pause"),
            &serde_json::json!({}),
        );
        assert_eq!(status, 200);
        assert_eq!(paused["paused"], true);
        assert_eq!(paused["status"], "paused");
        assert_eq!(paused["enabled"], true);
        assert!(
            paused["tasks"]
                .as_array()
                .is_some_and(|tasks| tasks.iter().all(|task| {
                    task["due"] == false && task["skip_reason"] == "scheduler_paused"
                })),
            "paused scheduler should not mark any task due: {paused}"
        );
        let (status, config_after_pause) = get_json(&agent, &config_url);
        assert_eq!(status, 200);
        assert_eq!(
            config_after_pause["effective"]["enabled"], true,
            "scheduler pause must not disable automation config"
        );
        let (status, resumed) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/scheduler/resume"),
            &serde_json::json!({}),
        );
        assert_eq!(status, 200);
        assert_eq!(resumed["paused"], false);
        assert_eq!(resumed["status"], "configured");

        let hermes_patch = serde_json::json!({
            "host_mode": "delegated_host"
        });
        let (status, saved) = patch_json_body(&agent, &config_url, &hermes_patch);
        assert_eq!(status, 200);
        assert_eq!(saved["effective"]["host_mode"], "delegated_host");
        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["features"]["automation"], true);
        assert_eq!(
            capabilities["features"]["llm_curation"],
            false,
            "delegated-host mode delegates intelligence and must not advertise TraceDecay-owned LLM curation"
        );
        assert_eq!(capabilities["automation"]["mode"], "delegated_host");
        assert_eq!(capabilities["automation"]["backend"], "codex_app_server");
        assert_eq!(capabilities["automation"]["host_mode"], "delegated_host");

        let legacy_host_mode_patch = serde_json::json!({
            "host_mode": "hermes_hosted"
        });
        let (status, legacy_saved) =
            patch_json_body(&agent, &config_url, &legacy_host_mode_patch);
        assert_eq!(status, 200);
        assert_eq!(
            legacy_saved["effective"]["host_mode"],
            "delegated_host",
            "legacy hermes_hosted config must normalize to the provider-agnostic delegated_host mode"
        );

        let external_patch = serde_json::json!({
            "backend": "external_command",
            "host_mode": "standalone"
        });
        let (status, rejected) = patch_json_body(&agent, &config_url, &external_patch);
        assert_eq!(status, 400);
        assert_eq!(rejected["validation_errors"][0]["field"], "backend");
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("external_command")),
            "external backend rejection should explain the unsupported backend: {rejected}"
        );
        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["features"]["automation"], true);
        assert_eq!(capabilities["features"]["llm_curation"], false);
        assert_eq!(capabilities["automation"]["mode"], "delegated_host");
        assert_eq!(capabilities["automation"]["backend"], "codex_app_server");
        assert_eq!(capabilities["automation"]["host_mode"], "delegated_host");

        let (status, saved_auto_apply) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({
                "require_dashboard_approval": false,
                "auto_apply_memory_ops": true
            }),
        );
        assert_eq!(
            status, 200,
            "explicit memory auto-apply should save: {saved_auto_apply}"
        );
        assert_eq!(
            saved_auto_apply["effective"]["require_dashboard_approval"],
            false
        );
        assert_eq!(saved_auto_apply["effective"]["auto_apply_memory_ops"], true);

        let (status, rejected) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({
                "modle": "typo-model"
            }),
        );
        assert_eq!(status, 400);
        assert_eq!(rejected["validation_errors"][0]["field"], "modle");
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("unknown field `modle`")),
            "unknown top-level field should be rejected clearly: {rejected}"
        );

        let (status, rejected) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({
                "memory_curator": { "schedul": "manual" }
            }),
        );
        assert_eq!(status, 400);
        assert_eq!(rejected["validation_errors"][0]["field"], "schedul");
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("unknown field `schedul`")),
            "unknown nested task field should be rejected clearly: {rejected}"
        );
        server.stop();

        let cg = TraceDecay::open(&project_root)
            .await
            .unwrap_or_else(|err| panic!("failed to reopen fixture project: {err}"));
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;
        let (status, restored) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/config"),
        );
        assert_eq!(status, 200);
        assert_eq!(restored["project"]["model"], "project-model");
        assert_eq!(restored["effective"]["model"], "project-model");
        assert_eq!(
            restored["effective"]["tasks"]["memory_curator"]["enabled"],
            true
        );
        let (status, reset) = delete_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/config"),
        );
        assert_eq!(status, 200);
        assert!(reset["project"].is_null());
        assert_eq!(reset["effective"]["model"], "global-model");
        assert_eq!(
            reset["effective"]["tasks"]["memory_curator"]["enabled"],
            false
        );
        assert!(!sidecar.exists(), "DELETE must remove project sidecar");
        let (status, reset_capabilities) =
            get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(reset_capabilities["automation"]["mode"], "standalone_backend");
        assert_eq!(
            reset_capabilities["automation"]["backend"],
            "codex_app_server"
        );
        server.stop();
    });
}

#[test]
fn managed_skills_are_dashboard_controllable_and_persistent() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();
        let base_url = &fixture.base_url;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["features"]["managed_skills"], true);

        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, empty) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(empty["count"], 0);
        assert_eq!(empty["skills"].as_array().map(Vec::len), Some(0));

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repo Hygiene",
            "summary": "Keep repository maintenance tasks consistent.",
            "category": "workflow",
            "body_markdown": "Use this when cleaning generated changes.",
            "support_files": [
                {
                    "path": "references/checklist.md",
                    "bytes": [99, 104, 101, 99, 107]
                }
            ],
            "provenance": {
                "source": "automation_run",
                "actor": "dashboard-test",
                "run_id": "run-dashboard-1"
            }
        });
        let (status, created) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/skills/draft"),
            &draft,
        );
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["id"], "repo-hygiene");
        assert_eq!(created["skill"]["metadata"]["state"], "pending_approval");
        assert!(created["skill"]["metadata"]["created_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert!(created["skill"]["metadata"]["updated_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert_eq!(created["usage_summary"]["view_count"], 0);
        assert_eq!(
            created["skill"]["metadata"]["provenance"]["run_id"],
            "run-dashboard-1"
        );
        let profile_root = tracedecay::storage::default_profile_root().unwrap();
        let skill = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        tracedecay::automation::skill_usage::record_skill_usage(
            &profile_root,
            &skill,
            tracedecay::automation::skill_usage::SkillUsageAction::Use,
            "dashboard-test",
            vec!["cursor".to_string(), "codex".to_string()],
            Some("cursor".to_string()),
            None,
        )
        .await
        .unwrap();
        let global_db = GlobalDb::open()
            .await
            .expect("dashboard fixture global db opens");
        global_db
            .append_analytics_event(&tracedecay::global_db::AnalyticsEventInsert {
                provider: "mcp".to_string(),
                project_id: GlobalDb::canonical_project_key(&fixture.project_root),
                session_id: Some("dashboard-skill-session".to_string()),
                timestamp: tracedecay::tracedecay::current_timestamp(),
                event_kind: "mcp_tool_call".to_string(),
                hook_name: None,
                tool_name: Some("tracedecay_skill_view".to_string()),
                tool_category: None,
                skill_name: None,
                hint_category: None,
                hint_id: None,
                outcome: Some("success".to_string()),
                metadata_json: Some(
                    serde_json::json!({
                        "function": {
                            "name": "tracedecay_skill_view",
                            "arguments": { "id": "repo-hygiene" }
                        }
                    })
                    .to_string(),
                ),
            })
            .await
            .unwrap();

        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");
        assert_eq!(listed["usage_summaries"][0]["view_count"], 1);
        assert_eq!(listed["usage_summaries"][0]["use_count"], 1);
        assert_eq!(
            listed["usage_summaries"][0]["targets"],
            serde_json::json!(["codex", "cursor", "mcp"])
        );
        assert_eq!(listed["stale_recommendations"][0]["skill_id"], "repo-hygiene");
        assert_eq!(listed["stale_recommendations"][0]["stale"], false);
        assert_eq!(listed["stale_recommendations"][0]["recommendation"], "keep");
        assert_eq!(
            listed["improvement_recommendations"][0]["skill_id"],
            "repo-hygiene"
        );
        assert_eq!(
            listed["improvement_recommendations"][0]["recommendation"],
            "none"
        );

        let skill_url = format!("{base_url}/api/automation/skills/repo-hygiene");
        let (status, viewed) = get_json(&agent, &skill_url);
        assert_eq!(status, 200);
        assert_eq!(
            viewed["skill"]["body_markdown"],
            "Use this when cleaning generated changes."
        );
        assert_eq!(viewed["usage_summary"]["use_count"], 1);
        assert_eq!(viewed["stale_recommendation"]["recommendation"], "keep");
        assert_eq!(viewed["improvement_recommendation"]["recommendation"], "none");

        let (status, approved) = post_json(&agent, &format!("{skill_url}/approve"));
        assert_eq!(status, 200);
        assert_eq!(approved["skill"]["metadata"]["state"], "active");
        assert_eq!(
            approved["skill"]["metadata"]["created_at"],
            created["skill"]["metadata"]["created_at"]
        );
        assert!(
            approved["skill"]["metadata"]["updated_at"]
                .as_i64()
                .unwrap_or_default()
                >= created["skill"]["metadata"]["updated_at"]
                    .as_i64()
                    .unwrap_or_default()
        );

        let (status, missing_checksum) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "summary": "Updated after dashboard review.",
                "body_markdown": "Use this when cleaning generated changes and record focused checks.",
                "pinned": true
            }),
        );
        assert_eq!(status, 400);
        assert!(missing_checksum["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("base_checksum")));

        let (status, patched) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "base_checksum": approved["skill"]["metadata"]["checksum"],
                "summary": "Updated after dashboard review.",
                "body_markdown": "Use this when cleaning generated changes and record focused checks.",
                "pinned": true
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(
            patched["skill"]["metadata"]["summary"],
            "Keep repository maintenance tasks consistent."
        );
        assert_eq!(patched["skill"]["metadata"]["state"], "active");
        assert_eq!(patched["skill"]["metadata"]["pinned"], false);
        assert_eq!(
            patched["skill"]["pending_update"]["metadata"]["summary"],
            "Updated after dashboard review."
        );
        assert_eq!(patched["skill"]["pending_update"]["metadata"]["pinned"], true);
        assert_eq!(
            patched["skill"]["pending_update"]["base_checksum"],
            approved["skill"]["metadata"]["checksum"]
        );
        assert_eq!(
            patched["skill"]["metadata"]["created_at"],
            created["skill"]["metadata"]["created_at"]
        );

        for (action, expected_state) in [
            ("approve", "active"),
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "pending_approval"),
        ] {
            let (status, updated) = post_json(&agent, &format!("{skill_url}/{action}"));
            assert_eq!(status, 200, "{action} should succeed");
            assert_eq!(updated["skill"]["metadata"]["state"], expected_state);
        }

        let persisted = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        assert_eq!(
            persisted.metadata.state,
            tracedecay::automation::managed_skills::ManagedSkillState::PendingApproval
        );
    });
}

#[test]
fn managed_skills_are_dashboard_controllable_with_explicit_approval() {
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
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, initial) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(initial["count"], 0);

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repository hygiene",
            "summary": "Keep repository checks focused.",
            "category": "maintenance",
            "body_markdown": "Run focused tests before broad suites.",
            "pinned": true
        });
        let (status, created) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["state"], "pending_approval");
        assert_eq!(created["skill"]["metadata"]["pinned"], true);
        assert_eq!(
            created["skill"]["metadata"]["provenance"]["source"],
            "user_draft"
        );

        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");
        assert_eq!(listed["skills"][0]["metadata"]["state"], "pending_approval");

        let skill_url = format!("{base_url}/api/automation/skills/repo-hygiene");
        let (status, updated) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "summary": "Updated with review evidence.",
                "body_markdown": "Record the narrow command that covers each change."
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(
            updated["skill"]["metadata"]["summary"],
            "Updated with review evidence."
        );
        assert_eq!(updated["skill"]["metadata"]["state"], "pending_approval");

        for (action, expected_state) in [
            ("approve", "active"),
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "pending_approval"),
        ] {
            let (status, payload) = post_json_body(
                &agent,
                &format!("{base_url}/api/automation/skills/repo-hygiene/{action}"),
                &serde_json::json!({}),
            );
            assert_eq!(status, 200, "{action} should succeed: {payload}");
            assert_eq!(payload["skill"]["metadata"]["state"], expected_state);
        }

        let skill_dir = profile_root
            .join("agent_managed")
            .join("skills")
            .join("repo-hygiene");
        assert!(skill_dir.join("skill.json").is_file());
        assert!(skill_dir.join("SKILL.md").is_file());
        server.stop();
    });
}

#[test]
fn automation_config_patch_does_not_rewrite_invalid_project_sidecar() {
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
        let sidecar = cg
            .store_layout()
            .dashboard_root
            .join("automation_config.json");
        let invalid_config = br#"{"enabled":true,"modle":"typo"}"#;
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, invalid_config).unwrap();

        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let (status, rejected) = patch_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/config"),
            &serde_json::json!({ "timeout_secs": 120 }),
        );
        assert_eq!(status, 500);
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("failed to parse automation config")),
            "invalid persisted config should block PATCH with a parse error: {rejected}"
        );
        assert_eq!(
            std::fs::read(&sidecar).unwrap(),
            invalid_config,
            "failed PATCH must not rewrite the invalid sidecar"
        );

        server.stop();
    });
}

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
                "evidence_limit": 7
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
                "storage_scope": "project_local"
            }))
            .expect("skill-writing request with unsupported field should receive response");
        let rejected_skill_status = rejected_skill_shape.status().as_u16();
        let rejected_skill_body = rejected_skill_shape
            .body_mut()
            .read_to_string()
            .expect("skill-writing rejection body should be readable");
        assert_eq!(rejected_skill_status, 422);
        assert!(
            rejected_skill_body.contains("storage_scope"),
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
        let mut session_terminal = false;
        for _ in 0..400 {
            records = tracedecay::automation::run_ledger::load_run_records(&dashboard_root, 10)
                .await
                .unwrap();
            session_terminal = records.iter().any(|record| {
                record.run_id == session_run_id && record.status.is_terminal()
            });
            if session_terminal {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            session_terminal,
            "session-reflector job did not reach a terminal record: {records:#?}"
        );

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
fn managed_skill_dashboard_api_persists_and_updates_lifecycle() {
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
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repository hygiene",
            "summary": "Keep repository maintenance guidance current.",
            "category": "maintenance",
            "body_markdown": "Use focused checks before changing generated files.",
            "support_files": [
                {
                    "path": "references/checklist.md",
                    "bytes": [45, 32, 114, 117, 110, 32, 116, 101, 115, 116, 115, 10]
                }
            ],
            "provenance": {
                "source": "user_draft",
                "actor": "dashboard",
                "run_id": null
            }
        });
        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, created) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["state"], "pending_approval");
        assert!(created["skill"]["metadata"]["created_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert!(created["skill"]["metadata"]["updated_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert!(
            profile_root
                .join("agent_managed/skills/repo-hygiene/SKILL.md")
                .is_file(),
            "drafting a managed skill must persist a SKILL.md package"
        );

        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");

        let (status, viewed) = get_json(
            &agent,
            &format!("{base_url}/api/automation/skills/repo-hygiene"),
        );
        assert_eq!(status, 200);
        assert_eq!(viewed["skill"]["metadata"]["id"], "repo-hygiene");

        for (action, expected_state) in [
            ("approve", "active"),
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "pending_approval"),
        ] {
            let (status, response) = post_json(
                &agent,
                &format!("{base_url}/api/automation/skills/repo-hygiene/{action}"),
            );
            assert_eq!(status, 200);
            assert_eq!(response["skill"]["metadata"]["state"], expected_state);
        }
        server.stop();
    });
}

#[test]
fn managed_skill_dashboard_api_controls_staged_updates() {
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
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repository hygiene",
            "summary": "Keep repository maintenance guidance current.",
            "category": "maintenance",
            "body_markdown": "Use focused checks before changing generated files.",
            "support_files": [
                {
                    "path": "references/checklist.md",
                    "bytes": [45, 32, 114, 117, 110, 32, 116, 101, 115, 116, 115, 10]
                }
            ],
            "provenance": {
                "source": "user_draft",
                "actor": "dashboard",
                "run_id": null
            }
        });
        let skills_url = format!("{base_url}/api/automation/skills");
        let skill_url = format!("{skills_url}/repo-hygiene");
        let (status, _) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        let (status, _) = post_json(&agent, &format!("{skill_url}/approve"));
        assert_eq!(status, 200);

        let active = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        let base_checksum = active.metadata.checksum.clone();
        tracedecay::automation::managed_skills::stage_managed_skill_update(
            &profile_root,
            "repo-hygiene",
            &base_checksum,
            tracedecay::automation::managed_skills::ManagedSkillUpdate {
                summary: Some("Stage dashboard-visible generated guidance.".to_string()),
                body_markdown: Some(
                    "Review the run ledger before applying generated edits.".to_string(),
                ),
                support_files: Some(vec![
                    tracedecay::automation::managed_skills::ManagedSupportFile::new(
                        "templates/review.md",
                        b"review body".to_vec(),
                    )
                    .unwrap(),
                ]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (status, staged_view) = get_json(&agent, &skill_url);
        assert_eq!(status, 200);
        assert_eq!(staged_view["skill"]["metadata"]["state"], "active");
        assert_eq!(
            staged_view["skill"]["metadata"]["summary"],
            "Keep repository maintenance guidance current."
        );
        assert_eq!(
            staged_view["skill"]["pending_update"]["metadata"]["summary"],
            "Stage dashboard-visible generated guidance."
        );
        let skill_dir = profile_root.join("agent_managed/skills/repo-hygiene");
        assert!(skill_dir.join("references/checklist.md").is_file());
        assert!(!skill_dir.join("templates/review.md").exists());

        let (status, discarded) = post_json(&agent, &format!("{skill_url}/discard-update"));
        assert_eq!(status, 200);
        assert!(discarded["skill"]["pending_update"].is_null());
        assert_eq!(
            discarded["skill"]["metadata"]["summary"],
            "Keep repository maintenance guidance current."
        );

        let active = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        tracedecay::automation::managed_skills::stage_managed_skill_update(
            &profile_root,
            "repo-hygiene",
            &active.metadata.checksum,
            tracedecay::automation::managed_skills::ManagedSkillUpdate {
                summary: Some("Approve dashboard-visible generated guidance.".to_string()),
                body_markdown: Some(
                    "Review the run ledger before applying generated edits.".to_string(),
                ),
                support_files: Some(vec![
                    tracedecay::automation::managed_skills::ManagedSupportFile::new(
                        "templates/review.md",
                        b"review body".to_vec(),
                    )
                    .unwrap(),
                ]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (status, approved) = post_json(&agent, &format!("{skill_url}/approve"));
        assert_eq!(status, 200);
        assert_eq!(approved["skill"]["metadata"]["state"], "active");
        assert_eq!(
            approved["skill"]["metadata"]["summary"],
            "Approve dashboard-visible generated guidance."
        );
        assert!(approved["skill"]["pending_update"].is_null());
        assert!(!skill_dir.join("references/checklist.md").exists());
        assert!(skill_dir.join("templates/review.md").is_file());

        server.stop();
    });
}

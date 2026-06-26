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

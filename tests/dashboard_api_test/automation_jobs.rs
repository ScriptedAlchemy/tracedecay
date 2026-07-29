use crate::dashboard_api_support::*;

#[test]
fn dashboard_three_request_chain_cannot_enable_and_run_a_shell_command() {
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
        let marker = tmp_root.join("dashboard-command-ran");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
        let missing_codex_bin = tmp_root.join("missing-codex");
        let _codex_bin_guard = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &missing_codex_bin);

        let mut global_config = tracedecay::user_config::UserConfig::default();
        global_config.automation.enabled = true;
        global_config.automation.backend =
            tracedecay::automation::config::AutomationBackend::CodexAppServer;
        global_config
            .save()
            .expect("global user config should save");

        let (cg, host_runtime) = setup_project(&project_root).await;
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

        let config_url = format!("{base_url}/api/plugins/holographic/curation/config");
        let (status, rejected) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({ "allow_job_commands": true }),
        );
        assert_eq!(status, 400, "{rejected}");

        #[cfg(windows)]
        let command = format!("echo exploited> \"{}\"", marker.display());
        #[cfg(not(windows))]
        let command = format!("printf exploited > '{}'", marker.display());
        let jobs_url = format!("{base_url}/api/automation/jobs");
        let (status, created) = post_json_body(
            &agent,
            &jobs_url,
            &serde_json::json!({
                "id": "untrusted-command",
                "name": "Untrusted command",
                "prompt": "Do nothing.",
                "enabled": true,
                "pre_run_command": command,
            }),
        );
        assert_eq!(status, 200, "{created}");

        let (status, accepted) = post_json_body(
            &agent,
            &format!("{jobs_url}/untrusted-command/run"),
            &serde_json::json!({}),
        );
        assert_eq!(status, 202, "{accepted}");
        for _ in 0..50 {
            if marker.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !marker.exists(),
            "dashboard HTTP must not enable the pre-run shell command"
        );

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert!(capabilities["features"].is_object(), "{capabilities}");

        server.stop();
    });
}

#[test]
fn automation_jobs_crud_and_manual_run_are_dashboard_controllable() {
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

        let (cg, host_runtime) = setup_project(&project_root).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();
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

        let jobs_url = format!("{base_url}/api/automation/jobs");
        let (status, empty) = get_json(&agent, &jobs_url);
        assert_eq!(status, 200);
        assert_eq!(empty["count"], 0);

        let create = serde_json::json!({
            "id": "daily-digest",
            "name": "Daily digest",
            "prompt": "Summarize the project changes.",
            "schedule": "*/15 * * * *",
            "enabled": true,
            "skill_ids": ["automation-run-review"],
            "delivery": { "mode": "file" }
        });
        let (status, created) = post_json_body(&agent, &jobs_url, &create);
        assert_eq!(status, 200);
        assert_eq!(created["job"]["id"], "daily-digest");
        assert_eq!(created["job"]["schedule"], "*/15 * * * *");
        assert_eq!(created["job"]["delivery"]["mode"], "file");

        let sidecar = dashboard_root.join("automation_jobs.json");
        assert!(sidecar.exists(), "job create must persist the jobs sidecar");

        let (status, listed) = get_json(&agent, &jobs_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["jobs"][0]["id"], "daily-digest");

        let job_url = format!("{jobs_url}/daily-digest");
        let patch = serde_json::json!({
            "schedule": null,
            "interval_secs": null,
            "pre_run_command": "printf dashboard",
            "delivery": {
                "mode": "webhook",
                "url": "https://example.test/hook"
            }
        });
        let (status, updated) = patch_json_body(&agent, &job_url, &patch);
        assert_eq!(status, 200);
        assert!(updated["job"]["schedule"].is_null());
        assert!(updated["job"]["interval_secs"].is_null());
        assert_eq!(updated["job"]["pre_run_command"], "printf dashboard");
        assert_eq!(updated["job"]["delivery"]["mode"], "webhook");

        let (status, rejected) = post_json_body(
            &agent,
            &jobs_url,
            &serde_json::json!({
                "id": "../escape",
                "name": "bad",
                "prompt": "bad"
            }),
        );
        assert_eq!(status, 400);
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("must be 1-64 characters")),
            "invalid job id should be rejected clearly: {rejected}"
        );

        let run_url = format!("{job_url}/run");
        let (status, accepted) = post_json_body(&agent, &run_url, &serde_json::json!({}));
        assert_eq!(status, 202);
        assert_eq!(accepted["job_id"], "daily-digest");
        assert_eq!(accepted["task"], "user_job:daily-digest");
        assert_eq!(accepted["status"], "accepted");

        let (status, deleted) = delete_json(&agent, &job_url);
        assert_eq!(status, 200);
        assert_eq!(deleted["deleted"], "daily-digest");
        let (status, listed) = get_json(&agent, &jobs_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 0);

        server.stop();
    });
}

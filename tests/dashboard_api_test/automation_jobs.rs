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
            tracedecay_agent_hosts::automation::config::AutomationBackend::CodexAppServer;
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

#[test]
fn dashboard_user_job_history_appears_only_after_retained_settlement() {
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
        let backend_started = tmp_root.join("user-job-backend-started");
        let release_backend = tmp_root.join("release-user-job-backend");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let fake_codex_root = tmp_root.join("fake-codex");
        let fake_codex_script = fake_codex_root.join("codex.py");
        let fake_codex_bin = fake_codex_bin(&fake_codex_root);
        let script = r#"#!/usr/bin/env python3
import json
import os
import pathlib
import sys
import time

if len(sys.argv) != 2 or sys.argv[1] != "app-server":
    sys.exit(42)
if os.environ.get("TRACEDECAY_CODEX_SUMMARY_CHILD") != "1":
    sys.exit(43)
started = pathlib.Path(__STARTED_PATH__)
release = pathlib.Path(__RELEASE_PATH__)

for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        print(json.dumps({"id": msg.get("id"), "result": {}}), flush=True)
    elif method == "thread/start":
        print(json.dumps({
            "id": msg.get("id"),
            "result": {"thread": {"id": "thread-dashboard-user-job", "model": "dashboard-fake-model"}}
        }), flush=True)
    elif method == "turn/start":
        started.write_text("started\n")
        while not release.exists():
            time.sleep(0.01)
        print(json.dumps({
            "method": "item/agentMessage/delta",
            "params": {"delta": "retained dashboard user job output", "model": "dashboard-fake-model"}
        }), flush=True)
        print(json.dumps({"method": "turn/completed"}), flush=True)
        break
"#
        .replace(
            "__STARTED_PATH__",
            &serde_json::to_string(&backend_started.display().to_string())
                .unwrap_or_else(|error| panic!("encode backend-started path: {error}")),
        )
        .replace(
            "__RELEASE_PATH__",
            &serde_json::to_string(&release_backend.display().to_string())
                .unwrap_or_else(|error| panic!("encode backend-release path: {error}")),
        );
        write_file(&fake_codex_script, &script);
        install_fake_codex_launcher(&fake_codex_script, &fake_codex_bin);
        let _codex_bin_guard = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &fake_codex_bin);

        let mut global_config = tracedecay::user_config::UserConfig::default();
        global_config.automation.enabled = true;
        global_config.automation.backend =
            tracedecay_agent_hosts::automation::config::AutomationBackend::CodexAppServer;
        global_config
            .save()
            .expect("global user config should save");

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
        let (status, created) = post_json_body(
            &agent,
            &jobs_url,
            &serde_json::json!({
                "id": "retained-history",
                "name": "Retained history",
                "prompt": "Produce the retained history fixture.",
                "enabled": true,
                "delivery": { "mode": "file" }
            }),
        );
        assert_eq!(status, 200, "{created}");

        let (status, accepted) = post_json_body(
            &agent,
            &format!("{jobs_url}/retained-history/run"),
            &serde_json::json!({}),
        );
        assert_eq!(status, 202, "{accepted}");
        let run_id = accepted["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("accepted run must expose its exact id: {accepted}"));

        for _ in 0..250 {
            if backend_started.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let started = backend_started.exists();
        let unsettled =
            tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded(
                &dashboard_root,
                run_id,
            )
            .await;
        // Always release the child before asserting so a failed falsifier
        // cannot leave the dashboard server waiting on the fixture backend.
        write_file(&release_backend, "release\n");
        assert!(
            started,
            "user-job backend must reach admitted execution; ledger state: {unsettled:?}"
        );
        assert!(
            unsettled
                .unwrap_or_else(|error| panic!("read unsettled user-job history: {error}"))
                .is_none(),
            "admitted execution must not expose ledger history before outer settlement"
        );

        let mut settled = None;
        for _ in 0..250 {
            settled =
                tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded(
                    &dashboard_root,
                    run_id,
                )
                .await
                .unwrap_or_else(|error| panic!("read settled user-job history: {error}"));
            if settled.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let settled = settled.unwrap_or_else(|| {
            panic!("retained dashboard user-job history must appear after settlement")
        });
        assert_eq!(settled.run_id, run_id);
        assert_eq!(
            settled.task,
            tracedecay_agent_hosts::automation::backend::AgentTaskKind::UserJob
        );
        assert_eq!(settled.task_key.as_deref(), Some("user_job:retained-history"));
        assert_eq!(
            settled.trigger,
            tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger::Dashboard
        );
        assert_eq!(
            settled.status,
            tracedecay_agent_hosts::automation::run_ledger::AutomationRunStatus::Succeeded
        );
        let records =
            tracedecay_agent_hosts::automation::run_ledger::load_run_records(&dashboard_root, 16)
                .await
                .unwrap_or_else(|error| panic!("read settled dashboard user-job ledger: {error}"));
        assert_eq!(
            records
                .iter()
                .filter(|record| record.run_id == run_id)
                .count(),
            1,
            "retained settlement must publish one logical row for the admitted run"
        );

        server.stop();
    });
}

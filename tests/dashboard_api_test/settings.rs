use crate::dashboard_api_support::*;
use serde_json::json;

#[test]
fn settings_dashboard_api_aggregates_and_updates_config() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();
        let url = format!("{}/api/settings", fixture.base_url);

        let (status, settings) = get_json(&agent, &url);
        assert_eq!(status, 200, "GET settings failed: {settings}");

        assert_eq!(settings["project"]["config"]["git_ignore"], true);
        assert_eq!(settings["project"]["config"]["extract_docstrings"], true);
        assert_eq!(settings["project"]["config"]["track_call_sites"], true);
        assert_eq!(settings["project"]["config"]["max_file_size"], 1_048_576);
        let exclude = settings["project"]["config"]["exclude"]
            .as_array()
            .unwrap_or_else(|| panic!("expected exclude array: {settings}"));
        assert!(
            exclude.iter().any(|glob| glob == "**/node_modules/**"),
            "default excludes should include node_modules: {settings}"
        );
        assert!(
            settings["project"]["config_path"]
                .as_str()
                .unwrap_or_default()
                .ends_with("config.json")
        );

        assert_eq!(settings["user"]["upload_enabled"], false);
        assert_eq!(settings["user"]["watcher_debounce"], "2s");
        assert_eq!(settings["user"]["extraction_timeout_secs"], 60);

        assert_eq!(
            settings["automation"]["config_endpoint"],
            "/api/plugins/holographic/curation/config"
        );

        assert_eq!(settings["storage"]["storage_mode"], "profile_sharded");
        assert!(
            !settings["storage"]["graph_db"]
                .as_str()
                .unwrap_or_default()
                .is_empty()
        );

        assert_eq!(settings["version"]["version"], env!("CARGO_PKG_VERSION"));
        let channel = settings["version"]["channel"].as_str().unwrap_or_default();
        assert!(
            channel == "stable" || channel == "beta",
            "unexpected channel: {channel}"
        );

        let variables = settings["environment"]["variables"]
            .as_array()
            .unwrap_or_else(|| panic!("expected environment variables array: {settings}"));
        for name in [
            "TRACEDECAY_ENABLE_GLOBAL_DB",
            "TRACEDECAY_DISABLE_GLOBAL_DB",
            "TRACEDECAY_OFFLINE",
            "TRACEDECAY_GLOBAL_DB",
            "TRACEDECAY_DATA_DIR",
        ] {
            let variable = variables
                .iter()
                .find(|variable| variable["name"] == name)
                .unwrap_or_else(|| panic!("missing env variable {name}: {settings}"));
            assert!(
                !variable["description"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty(),
                "env variable {name} needs a description"
            );
        }
        let global_db_var = variables
            .iter()
            .find(|variable| variable["name"] == "TRACEDECAY_GLOBAL_DB")
            .unwrap_or_else(|| panic!("missing TRACEDECAY_GLOBAL_DB"));
        assert_eq!(global_db_var["active"], true);
        assert!(settings["environment"]["global_accounting_enabled"].is_boolean());

        let project_url = format!("{url}/project");
        let (status, patched) = patch_json_body(
            &agent,
            &project_url,
            &json!({
                "exclude": ["target/**", "dist/**"],
                "include": [".github/**"],
                "max_file_size": 2048
            }),
        );
        assert_eq!(status, 200, "project patch failed: {patched}");
        assert_eq!(
            patched["resync_recommended"], true,
            "indexing changes must flag a re-sync: {patched}"
        );
        assert_eq!(patched["project"]["config"]["max_file_size"], 2048);
        assert_eq!(patched["project"]["config"]["include"][0], ".github/**");
        assert_eq!(
            patched["project"]["config"]["exclude"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(patched["project"]["config"]["git_ignore"], true);

        let (status, unchanged) =
            patch_json_body(&agent, &project_url, &json!({ "max_file_size": 2048 }));
        assert_eq!(status, 200, "no-op project patch failed: {unchanged}");
        assert_eq!(unchanged["resync_recommended"], false);

        let (status, invalid) =
            patch_json_body(&agent, &project_url, &json!({ "exclude": ["[invalid"] }));
        assert_eq!(status, 400, "invalid glob should 400: {invalid}");
        assert_eq!(invalid["validation_errors"][0]["field"], "exclude");
        assert!(
            invalid["validation_errors"][0]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("[invalid")
        );

        let (status, unknown) =
            patch_json_body(&agent, &project_url, &json!({ "made_up_field": true }));
        assert_eq!(status, 400, "unknown field should 400: {unknown}");
        assert_eq!(unknown["validation_errors"][0]["field"], "made_up_field");

        let (status, zero) = patch_json_body(&agent, &project_url, &json!({ "max_file_size": 0 }));
        assert_eq!(status, 400, "zero max_file_size should 400: {zero}");
        assert_eq!(zero["validation_errors"][0]["field"], "max_file_size");

        let user_url = format!("{url}/user");
        let (status, user) = patch_json_body(
            &agent,
            &user_url,
            &json!({
                "upload_enabled": false,
                "watcher_debounce": "15s"
            }),
        );
        assert_eq!(status, 200, "user patch failed: {user}");
        assert_eq!(
            user["restart_recommended"], true,
            "watcher debounce changes need a daemon restart: {user}"
        );
        assert_eq!(user["user"]["upload_enabled"], false);
        assert_eq!(user["user"]["watcher_debounce"], "15s");

        let (status, upload_only) =
            patch_json_body(&agent, &user_url, &json!({ "upload_enabled": true }));
        assert_eq!(status, 200, "upload-only patch failed: {upload_only}");
        assert_eq!(upload_only["restart_recommended"], false);

        let (status, bad_debounce) =
            patch_json_body(&agent, &user_url, &json!({ "watcher_debounce": "1h" }));
        assert_eq!(status, 400, "bad debounce should 400: {bad_debounce}");
        assert_eq!(
            bad_debounce["validation_errors"][0]["field"],
            "watcher_debounce"
        );

        let (status, reloaded) = get_json(&agent, &url);
        assert_eq!(status, 200);
        assert_eq!(reloaded["project"]["config"]["max_file_size"], 2048);
        assert_eq!(reloaded["project"]["config"]["include"][0], ".github/**");
        assert_eq!(reloaded["user"]["upload_enabled"], true);
        assert_eq!(reloaded["user"]["watcher_debounce"], "15s");
    });
}

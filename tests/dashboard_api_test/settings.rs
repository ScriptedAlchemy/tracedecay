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

        let (status, settings_envelope) = get_json(&agent, &url);
        assert_eq!(status, 200, "GET settings failed: {settings_envelope}");
        assert_eq!(settings_envelope["schema_revision"], 1);
        assert_eq!(settings_envelope["domain_state"], "ready");
        assert_eq!(settings_envelope["coverage"]["completeness"], "complete");
        let settings = settings_envelope["payload"].clone();

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
        assert_eq!(settings["project"]["legacy_config_read_only"], true);
        assert!(
            settings["project"]["configuration_snapshot_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "settings must expose the pinned resolved snapshot: {settings}"
        );
        assert!(
            settings["project"]["configuration_revision_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "settings must expose the pinned configuration revision: {settings}"
        );
        let revision = settings["project"]["configuration_revision_id"]
            .as_str()
            .unwrap_or_else(|| panic!("missing configuration revision: {settings}"))
            .to_owned();
        let user_revision = settings["user"]["configuration_revision_id"]
            .as_str()
            .unwrap_or_else(|| panic!("missing user settings revision: {settings}"))
            .to_owned();
        assert_eq!(
            user_revision, revision,
            "project and profile values must share one configuration revision"
        );
        let user_legacy_config_path = std::path::PathBuf::from(
            settings["user"]["legacy_config_path"]
                .as_str()
                .unwrap_or_else(|| panic!("missing legacy user config path: {settings}")),
        );
        let user_legacy_config_before = std::fs::read(&user_legacy_config_path).ok();
        assert_eq!(settings["user"]["legacy_config_read_only"], true);
        let legacy_config_path = std::path::PathBuf::from(
            settings["project"]["legacy_config_path"]
                .as_str()
                .unwrap_or_else(|| panic!("missing legacy config path: {settings}")),
        );
        assert!(
            !legacy_config_path.exists(),
            "fresh initialization must not create a writable config.json"
        );
        let legacy_config_before = std::fs::read(&legacy_config_path).ok();

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

        assert_eq!(
            settings["version"]["version"],
            tracedecay::version::build_version()
        );
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

        // Both editable scopes use the cataloged daemon configuration effect.
        // This fixture serves the dashboard directly, so neither scope may
        // advertise or perform a write.
        let legal_actions = settings_envelope["legal_actions"]
            .as_array()
            .unwrap_or_else(|| panic!("expected legal actions: {settings_envelope}"));
        let advertises = |operation: &str| {
            legal_actions
                .iter()
                .any(|action| action["kind"] == "request_apply" && action["operation"] == operation)
        };
        assert!(
            !advertises("configuration_batch"),
            "project apply must not be advertised without a control plane: {settings_envelope}"
        );
        assert!(!advertises("user_settings_mutate"));

        let project_url = format!("{url}/project");
        let (status, unchanged_envelope) = patch_json_body(
            &agent,
            &project_url,
            &json!({
                "expected_revision_id": revision,
                "idempotency_key": "configuration.idempotency.dashboard-project-noop",
                "max_file_size": 1_048_576
            }),
        );
        assert_eq!(
            status, 200,
            "no-op project patch failed: {unchanged_envelope}"
        );
        assert!(unchanged_envelope["application_outcome"].is_null());
        let unchanged = unchanged_envelope["current"]["payload"].clone();
        assert_eq!(unchanged["resync_recommended"], false);
        assert_eq!(
            unchanged["project"]["configuration_revision_id"], revision,
            "a no-op patch changes nothing, so the revision must stand: {unchanged}"
        );

        // A patch that really changes configuration needs the absent control
        // plane. It is a typed unavailable — never a fabricated success, and
        // never a write that quietly lands somewhere else.
        let (status, unavailable) = patch_json_body(
            &agent,
            &project_url,
            &json!({
                "expected_revision_id": revision,
                "idempotency_key": "configuration.idempotency.dashboard-project-unavailable",
                "exclude": ["target/**", "dist/**"],
                "include": [".github/**"],
                "max_file_size": 2048
            }),
        );
        assert_eq!(
            status, 503,
            "a project mutation without its authority must be unavailable: {unavailable}"
        );
        assert_eq!(unavailable["code"], "configuration_authority_unavailable");
        assert_eq!(
            std::fs::read(&legacy_config_path).ok(),
            legacy_config_before,
            "a rejected mutation must not fall back to config.json"
        );
        let (status, after_unavailable) = get_json(&agent, &url);
        assert_eq!(status, 200);
        assert_eq!(
            after_unavailable["payload"]["project"]["config"]["max_file_size"], 1_048_576,
            "an unavailable authority must leave configuration untouched: {after_unavailable}"
        );
        assert_eq!(
            after_unavailable["payload"]["project"]["configuration_revision_id"], revision,
            "an unavailable authority must not advance the revision: {after_unavailable}"
        );

        // A stale request cannot bypass the durable replay authority: without
        // that authority mounted, the route cannot truthfully decide whether
        // this is an exact replay or an unseen stale mutation.
        const FOREIGN_REVISION: &str = "configuration-revision-that-never-existed";
        let (status, stale) = patch_json_body(
            &agent,
            &project_url,
            &json!({
                "expected_revision_id": FOREIGN_REVISION,
                "idempotency_key": "configuration.idempotency.dashboard-project-stale",
                "track_call_sites": false
            }),
        );
        assert_eq!(
            status, 503,
            "stale project patch needs the replay authority: {stale}"
        );
        assert_eq!(stale["code"], "configuration_authority_unavailable");

        let (status, absent) =
            patch_json_body(&agent, &project_url, &json!({ "track_call_sites": false }));
        assert_eq!(
            status, 400,
            "missing project revision must be rejected: {absent}"
        );
        assert_eq!(
            absent["validation_errors"][0]["field"],
            "expected_revision_id"
        );

        let (status, absent_project_idempotency) = patch_json_body(
            &agent,
            &project_url,
            &json!({
                "expected_revision_id": revision,
                "track_call_sites": false
            }),
        );
        assert_eq!(
            status, 400,
            "missing project idempotency must be rejected: {absent_project_idempotency}"
        );
        assert_eq!(
            absent_project_idempotency["validation_errors"][0]["field"],
            "idempotency_key"
        );

        let (status, invalid) = patch_json_body(
            &agent,
            &project_url,
            &json!({
                "expected_revision_id": revision,
                "idempotency_key": "configuration.idempotency.dashboard-project-invalid",
                "exclude": ["[invalid"]
            }),
        );
        assert_eq!(status, 400, "invalid glob should 400: {invalid}");
        assert_eq!(invalid["validation_errors"][0]["field"], "exclude");
        assert!(
            invalid["validation_errors"][0]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("[invalid")
        );

        let (status, unknown) = patch_json_body(
            &agent,
            &project_url,
            &json!({
                "expected_revision_id": revision,
                "idempotency_key": "configuration.idempotency.dashboard-project-unknown",
                "made_up_field": true
            }),
        );
        assert_eq!(status, 400, "unknown field should 400: {unknown}");
        assert_eq!(unknown["validation_errors"][0]["field"], "made_up_field");

        let (status, zero) = patch_json_body(
            &agent,
            &project_url,
            &json!({
                "expected_revision_id": revision,
                "idempotency_key": "configuration.idempotency.dashboard-project-zero",
                "max_file_size": 0
            }),
        );
        assert_eq!(status, 400, "zero max_file_size should 400: {zero}");
        assert_eq!(zero["validation_errors"][0]["field"], "max_file_size");

        let user_url = format!("{url}/user");
        let (status, unavailable_user) = patch_json_body(
            &agent,
            &user_url,
            &json!({
                "expected_revision_id": user_revision,
                "idempotency_key": "configuration.idempotency.dashboard-user-unmounted",
                "upload_enabled": false,
                "watcher_debounce": "15s"
            }),
        );
        assert_eq!(
            status, 503,
            "user patch without the control plane must be unavailable: {unavailable_user}"
        );
        assert_eq!(
            std::fs::read(&user_legacy_config_path).ok(),
            user_legacy_config_before,
            "an unavailable profile mutation must not write config.toml"
        );

        let (status, absent_user_idempotency) = patch_json_body(
            &agent,
            &user_url,
            &json!({
                "expected_revision_id": user_revision,
                "upload_enabled": false
            }),
        );
        assert_eq!(
            status, 400,
            "missing user idempotency must be rejected: {absent_user_idempotency}"
        );
        assert_eq!(
            absent_user_idempotency["validation_errors"][0]["field"],
            "idempotency_key"
        );

        let (status, bad_debounce) = patch_json_body(
            &agent,
            &user_url,
            &json!({
                "expected_revision_id": user_revision,
                "idempotency_key": "configuration.idempotency.dashboard-user-invalid",
                "watcher_debounce": "1h"
            }),
        );
        assert_eq!(status, 400, "bad debounce should 400: {bad_debounce}");
        assert_eq!(
            bad_debounce["validation_errors"][0]["field"],
            "watcher_debounce"
        );

        let (status, reloaded_envelope) = get_json(&agent, &url);
        assert_eq!(status, 200);
        let reloaded = reloaded_envelope["payload"].clone();
        assert_eq!(reloaded["project"]["config"]["max_file_size"], 1_048_576);
        assert_eq!(reloaded["project"]["configuration_revision_id"], revision);
        assert_eq!(reloaded["user"]["configuration_revision_id"], user_revision);
        assert_eq!(reloaded["user"]["upload_enabled"], false);
        assert_eq!(reloaded["user"]["watcher_debounce"], "2s");
    });
}

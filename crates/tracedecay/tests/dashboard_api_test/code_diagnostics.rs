use crate::dashboard_api_support::*;
use serde_json::Value;

#[test]
fn code_diagnostics_dashboard_api_exposes_engines_and_applies_settings() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();
        let url = format!("{}/api/plugins/code-diagnostics", fixture.base_url);

        let (status, initial) = get_json(&agent, &url);
        assert_eq!(status, 200, "code-diagnostics read failed: {initial}");
        assert_eq!(initial["settings"]["idle_backfill"], "idle");
        assert!(
            engines(&initial)
                .iter()
                .any(|engine| engine["language"] == "rust"),
            "rust engine should be advertised"
        );
        assert_eq!(
            engine(&initial, "rust")["state"],
            "inactive",
            "fixture has .rs files but no Cargo.toml, so rust-analyzer should not auto-start"
        );
        let mut settings_revision = initial["settings_revision"]
            .as_str()
            .unwrap_or_else(|| panic!("missing settings revision: {initial}"))
            .to_owned();

        let (status, patched) = patch_json_body(
            &agent,
            &url,
            &serde_json::json!({
                "expected_revision": settings_revision,
                "idle_backfill": "off",
                "languages": {
                    "rust": {
                        "enabled": false,
                        "command_override": "/opt/tracedecay-test/rust-analyzer"
                    }
                }
            }),
        );
        assert_eq!(status, 200, "patch failed: {patched}");
        assert_eq!(patched["settings"]["idle_backfill"], "off");
        let rust_status = engine(&patched, "rust");
        assert_eq!(rust_status["enabled"], false);
        assert_eq!(rust_status["state"], "disabled");
        assert_eq!(rust_status["command"], "/opt/tracedecay-test/rust-analyzer");
        assert_eq!(rust_status["default_command"], "rust-analyzer");
        assert!(
            rust_status["install_options"]
                .as_array()
                .unwrap_or_else(|| panic!("expected install options"))
                .iter()
                .any(|option| option["command"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("rust-analyzer"))
        );
        settings_revision = patched["settings_revision"]
            .as_str()
            .unwrap_or_else(|| panic!("missing patched settings revision: {patched}"))
            .to_owned();

        let (status, refreshed) = post_json(&agent, &format!("{url}/refresh/rust"));
        assert_eq!(
            status, 400,
            "disabled refresh must be rejected before execution: {refreshed}"
        );
        assert!(
            refreshed["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("language is disabled")),
            "disabled refresh should explain the rejection: {refreshed}"
        );

        let (status, reloaded) = get_json(&agent, &url);
        assert_eq!(status, 200);
        assert_eq!(reloaded["settings"]["idle_backfill"], "off");
        assert_eq!(reloaded["settings"]["languages"]["rust"]["enabled"], false);
        assert_eq!(
            reloaded["settings"]["languages"]["rust"]["command_override"],
            "/opt/tracedecay-test/rust-analyzer"
        );

        let (status, toggled) = patch_json_body(
            &agent,
            &url,
            &serde_json::json!({
                "expected_revision": settings_revision,
                "languages": {
                    "rust": {
                        "enabled": true
                    }
                }
            }),
        );
        assert_eq!(status, 200, "toggle patch failed: {toggled}");
        assert_eq!(toggled["settings"]["languages"]["rust"]["enabled"], true);
        assert_eq!(
            toggled["settings"]["languages"]["rust"]["command_override"],
            "/opt/tracedecay-test/rust-analyzer"
        );
        settings_revision = toggled["settings_revision"]
            .as_str()
            .unwrap_or_else(|| panic!("missing toggled settings revision: {toggled}"))
            .to_owned();

        let (status, command_only) = patch_json_body(
            &agent,
            &url,
            &serde_json::json!({
                "expected_revision": settings_revision,
                "languages": {
                    "rust": {
                        "command_override": "/opt/tracedecay-test/rust-analyzer-2"
                    }
                }
            }),
        );
        assert_eq!(status, 200, "command patch failed: {command_only}");
        assert_eq!(
            command_only["settings"]["languages"]["rust"]["enabled"],
            true
        );
        assert_eq!(
            command_only["settings"]["languages"]["rust"]["command_override"],
            "/opt/tracedecay-test/rust-analyzer-2"
        );
        settings_revision = command_only["settings_revision"]
            .as_str()
            .unwrap_or_else(|| panic!("missing command settings revision: {command_only}"))
            .to_owned();

        let stale_revision = settings_revision.clone();
        let (status, cleared) = patch_json_body(
            &agent,
            &url,
            &serde_json::json!({
                "expected_revision": settings_revision,
                "languages": {
                    "rust": {
                        "command_override": null
                    }
                }
            }),
        );
        assert_eq!(status, 200, "clear patch failed: {cleared}");
        assert_eq!(cleared["settings"]["languages"]["rust"]["enabled"], true);
        assert_eq!(
            cleared["settings"]["languages"]["rust"]["command_override"],
            Value::Null
        );

        // The clear advanced the settings, so the revision the caller held
        // before it no longer describes them. A second writer arriving with it
        // must be told, not silently allowed to overwrite the clear.
        let (status, conflict) = patch_json_body(
            &agent,
            &url,
            &serde_json::json!({
                "expected_revision": stale_revision,
                "idle_backfill": "idle"
            }),
        );
        assert_eq!(
            status, 409,
            "stale settings patch must conflict: {conflict}"
        );
        assert_eq!(conflict["code"], "code_diagnostics_revision_conflict");
        assert_eq!(conflict["expected_revision"], stale_revision);
        assert_eq!(
            conflict["actual_revision"], cleared["settings_revision"],
            "the conflict must name the revision the authority actually holds: {conflict}"
        );

        let (status, unchanged) = get_json(&agent, &url);
        assert_eq!(status, 200);
        assert_eq!(
            unchanged["settings"]["idle_backfill"], "off",
            "a rejected patch must not apply any of its fields: {unchanged}"
        );
    });
}

fn engines(payload: &Value) -> &[Value] {
    payload["engines"]
        .as_array()
        .unwrap_or_else(|| panic!("expected engines array: {payload}"))
}

fn engine<'a>(payload: &'a Value, language: &str) -> &'a Value {
    engines(payload)
        .iter()
        .find(|engine| engine["language"] == language)
        .unwrap_or_else(|| panic!("expected {language} engine status: {payload}"))
}

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
        assert_eq!(status, 200);
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

        let (status, patched) = patch_json_body(
            &agent,
            &url,
            &serde_json::json!({
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
        assert!(rust_status["install_options"]
            .as_array()
            .unwrap_or_else(|| panic!("expected install options"))
            .iter()
            .any(|option| option["command"]
                .as_str()
                .unwrap_or_default()
                .contains("rust-analyzer")));

        let (status, refreshed) = post_json(&agent, &format!("{url}/refresh/rust"));
        assert_eq!(
            status, 200,
            "disabled refresh should be fail-open: {refreshed}"
        );
        let refreshed_rust = engine(&refreshed, "rust");
        assert_eq!(refreshed_rust["state"], "disabled");

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

        let (status, command_only) = patch_json_body(
            &agent,
            &url,
            &serde_json::json!({
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

        let (status, cleared) = patch_json_body(
            &agent,
            &url,
            &serde_json::json!({
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

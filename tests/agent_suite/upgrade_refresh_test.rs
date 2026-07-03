use std::path::Path;

use tempfile::TempDir;
use tracedecay::agents::{
    expected_tool_perms, AgentIntegration, ClaudeIntegration, InstallContext,
};

fn make_install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: expected_tool_perms(),
        profile: None,
        project_root: None,
        dashboard: false,
    }
}

/// Idempotency + unknown-key preservation for the Claude `.claude.json` case
/// specifically: a repeated `ClaudeIntegration.install` leaves that file
/// byte-identical and keeps user-owned keys/MCP entries intact. Other agents
/// (and Claude's `settings.json`) are not exercised here.
#[test]
fn claude_json_upgrade_refresh_is_idempotent_and_preserves_unknown_keys() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    let claude_json = home.join(".claude.json");

    std::fs::write(
        &claude_json,
        serde_json::to_string_pretty(&serde_json::json!({
            "theme": "dark",
            "customUserSetting": {"nested": [1, 2, 3]},
            "mcpServers": {
                "someOtherServer": {"command": "other"}
            }
        }))
        .unwrap(),
    )
    .unwrap();

    ClaudeIntegration.install(&ctx).unwrap();
    let after_first = std::fs::read_to_string(&claude_json).unwrap();

    ClaudeIntegration.install(&ctx).unwrap();
    let after_second = std::fs::read_to_string(&claude_json).unwrap();
    assert_eq!(
        after_first, after_second,
        "a repeated refresh must leave the config byte-identical"
    );

    let parsed: serde_json::Value = serde_json::from_str(&after_second).unwrap();
    assert_eq!(parsed["theme"], "dark");
    assert_eq!(
        parsed["customUserSetting"]["nested"],
        serde_json::json!([1, 2, 3])
    );
    assert!(parsed["mcpServers"]["someOtherServer"].is_object());
    assert!(
        parsed["mcpServers"]["tracedecay"].is_object(),
        "the tracedecay MCP entry must be present after refresh"
    );
}

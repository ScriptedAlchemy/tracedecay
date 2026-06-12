use std::io::Write;
use std::path::Path;

use tempfile::TempDir;
use tracedecay::agents::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, KiroIntegration,
};

fn make_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: Vec::new(),
        profile: None,
        project_root: None,
        dashboard: true,
    }
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn file_resource_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let path = percent_encode_file_uri_path(&path);
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn percent_encode_file_uri_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0F) as usize] as char);
            }
        }
    }
    encoded
}

fn assert_hook(
    agent: &serde_json::Value,
    event: &str,
    matcher: Option<&str>,
    subcommand: &str,
    timeout_ms: u64,
) {
    let hooks = agent["hooks"][event].as_array().unwrap();
    let hook = hooks
        .iter()
        .find(|hook| {
            let matcher_matches = match matcher {
                Some(expected) => hook["matcher"].as_str() == Some(expected),
                None => hook.get("matcher").is_none(),
            };
            matcher_matches
                && hook["command"]
                    .as_str()
                    .is_some_and(|command| command.contains(subcommand))
        })
        .unwrap_or_else(|| panic!("missing hook {event} {matcher:?} {subcommand}"));
    assert_eq!(hook["timeout_ms"].as_u64(), Some(timeout_ms));
}

#[test]
fn test_install_creates_global_mcp_steering_agent_and_default() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    KiroIntegration.install(&ctx).unwrap();

    let mcp_path = home.join(".kiro/settings/mcp.json");
    assert!(mcp_path.exists(), "global Kiro MCP config should exist");
    let mcp = read_json(&mcp_path);
    let server = &mcp["mcpServers"]["tracedecay"];
    assert!(server.is_object(), "mcpServers.tracedecay should exist");
    assert_eq!(
        server["command"].as_str(),
        Some("/usr/local/bin/tracedecay")
    );
    assert_eq!(
        server["args"].as_array().unwrap(),
        &[serde_json::json!("serve")]
    );
    assert_eq!(server["disabled"], serde_json::json!(false));
    assert!(
        server.get("autoApprove").is_none(),
        "global MCP config should leave approval policy to the managed Kiro agent"
    );

    let steering_path = home.join(".kiro/steering/tracedecay.md");
    assert!(
        steering_path.exists(),
        "global Kiro tracedecay.md should exist"
    );
    let steering = std::fs::read_to_string(&steering_path).unwrap();
    assert!(steering.contains("## Prefer tracedecay MCP tools"));
    assert!(steering.contains("delegate"));

    let agent_path = home.join(".kiro/agents/tracedecay.json");
    assert!(agent_path.exists(), "managed Kiro agent should exist");
    let agent = read_json(&agent_path);
    assert_eq!(agent["name"].as_str(), Some("tracedecay"));
    assert_eq!(agent["includeMcpJson"].as_bool(), Some(true));
    assert!(
        agent.get("prompt").is_none(),
        "managed agent should leave prompt unset so Kiro's default prompt is used"
    );
    let steering_resource = file_resource_uri(&steering_path);
    assert!(
        agent["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some(steering_resource.as_str())),
        "managed agent should load global tracedecay steering as an absolute resource"
    );
    assert_eq!(
        agent["tools"].as_array().unwrap(),
        &[serde_json::json!("*")]
    );
    assert_eq!(
        agent["allowedTools"].as_array().unwrap(),
        &[
            serde_json::json!("@builtin"),
            serde_json::json!("@tracedecay")
        ]
    );
    assert_hook(
        &agent,
        "userPromptSubmit",
        None,
        "hook-kiro-prompt-submit",
        5_000,
    );
    assert_hook(
        &agent,
        "preToolUse",
        Some("delegate"),
        "hook-kiro-pre-tool-use",
        5_000,
    );
    assert_hook(
        &agent,
        "preToolUse",
        Some("subagent"),
        "hook-kiro-pre-tool-use",
        5_000,
    );
    assert_hook(
        &agent,
        "postToolUse",
        Some("fs_write"),
        "hook-kiro-post-tool-use",
        30_000,
    );

    let cli_path = home.join(".kiro/settings/cli.json");
    assert!(cli_path.exists(), "Kiro CLI settings should exist");
    let cli = read_json(&cli_path);
    assert_eq!(cli["chat"]["defaultAgent"].as_str(), Some("tracedecay"));
}

#[test]
fn test_install_preserves_existing_mcp_config_and_writes_backup() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let mcp_path = home.join(".kiro/settings/mcp.json");
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mcp_path,
        r#"{"mcpServers":{"other":{"command":"other-bin"}},"theme":"dark"}"#,
    )
    .unwrap();

    let ctx = make_ctx(home);
    KiroIntegration.install(&ctx).unwrap();

    let mcp = read_json(&mcp_path);
    assert!(mcp["mcpServers"]["tracedecay"].is_object());
    assert!(mcp["mcpServers"]["other"].is_object());
    assert_eq!(mcp["theme"].as_str(), Some("dark"));
    assert!(
        home.join(".kiro/settings/mcp.json.bak").exists(),
        "install should preserve a backup before rewriting existing config"
    );
}

#[test]
fn test_install_and_uninstall_preserve_existing_steering_content() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    let user_steering_path = home.join(".kiro/steering/team.md");
    std::fs::create_dir_all(user_steering_path.parent().unwrap()).unwrap();
    std::fs::write(
        &user_steering_path,
        "## Existing Kiro guidance\n\nKeep this user-authored guidance.\n",
    )
    .unwrap();

    KiroIntegration.install(&ctx).unwrap();

    let user_steering = std::fs::read_to_string(&user_steering_path).unwrap();
    assert!(user_steering.contains("## Existing Kiro guidance"));
    assert!(user_steering.contains("Keep this user-authored guidance."));
    assert!(!user_steering.contains("## Prefer tracedecay MCP tools"));

    let tracedecay_steering_path = home.join(".kiro/steering/tracedecay.md");
    let installed = std::fs::read_to_string(&tracedecay_steering_path).unwrap();
    assert!(installed.contains("## Prefer tracedecay MCP tools"));

    KiroIntegration.uninstall(&ctx).unwrap();

    let uninstalled = std::fs::read_to_string(&user_steering_path).unwrap();
    assert!(uninstalled.contains("## Existing Kiro guidance"));
    assert!(uninstalled.contains("Keep this user-authored guidance."));
    assert!(!uninstalled.contains("## Prefer tracedecay MCP tools"));
    assert!(!tracedecay_steering_path.exists());
}

#[test]
fn test_uninstall_preserves_user_steering_after_tracedecay_block() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    KiroIntegration.install(&ctx).unwrap();

    let steering_path = home.join(".kiro/steering/tracedecay.md");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&steering_path)
        .unwrap()
        .write_all(b"\nUser guidance appended after setup without a new heading.\n")
        .unwrap();

    KiroIntegration.uninstall(&ctx).unwrap();

    let uninstalled = std::fs::read_to_string(&steering_path).unwrap();
    assert!(uninstalled.contains("User guidance appended after setup without a new heading."));
    assert!(!uninstalled.contains("## Prefer tracedecay MCP tools"));
}

#[test]
fn test_uninstall_removes_tracedecay_and_preserves_other_mcp_servers() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    let mcp_path = home.join(".kiro/settings/mcp.json");
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mcp_path,
        r#"{"mcpServers":{"other":{"command":"other-bin"}},"theme":"dark"}"#,
    )
    .unwrap();

    KiroIntegration.install(&ctx).unwrap();
    KiroIntegration.uninstall(&ctx).unwrap();

    let mcp = read_json(&mcp_path);
    assert!(mcp["mcpServers"]["other"].is_object());
    assert!(mcp["mcpServers"].get("tracedecay").is_none());
    assert_eq!(mcp["theme"].as_str(), Some("dark"));

    assert!(!home.join(".kiro/agents/tracedecay.json").exists());
    let cli = std::fs::read_to_string(home.join(".kiro/settings/cli.json")).unwrap_or_default();
    assert!(
        !cli.contains("defaultAgent"),
        "uninstall should remove tracedecay default agent"
    );
    let steering =
        std::fs::read_to_string(home.join(".kiro/steering/tracedecay.md")).unwrap_or_default();
    assert!(!steering.contains("## Prefer tracedecay MCP tools"));
}

#[test]
fn test_install_and_uninstall_preserve_user_managed_custom_agent() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    let agent_path = home.join(".kiro/agents/tracedecay.json");
    std::fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
    let custom_agent = serde_json::json!({
        "name": "tracedecay",
        "description": "User-managed custom agent",
        "includeMcpJson": true,
        "hooks": {
            "preToolUse": [
                {
                    "matcher": "delegate",
                    "command": "echo user-managed hook"
                }
            ]
        }
    });
    std::fs::write(
        &agent_path,
        serde_json::to_string_pretty(&custom_agent).unwrap(),
    )
    .unwrap();

    KiroIntegration.install(&ctx).unwrap();
    assert_eq!(read_json(&agent_path), custom_agent);
    assert!(
        !home.join(".kiro/settings/cli.json").exists(),
        "install should not point defaultAgent at a user-managed tracedecay agent"
    );

    KiroIntegration.uninstall(&ctx).unwrap();
    assert_eq!(read_json(&agent_path), custom_agent);
}

#[test]
fn test_install_preserves_existing_custom_default_agent_choice() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    let cli_path = home.join(".kiro/settings/cli.json");
    std::fs::create_dir_all(cli_path.parent().unwrap()).unwrap();
    std::fs::write(
        &cli_path,
        r#"{"chat":{"defaultAgent":"my-team-agent"},"telemetry":{"enabled":false}}"#,
    )
    .unwrap();

    KiroIntegration.install(&ctx).unwrap();

    let cli = read_json(&cli_path);
    assert_eq!(cli["chat"]["defaultAgent"].as_str(), Some("my-team-agent"));
    assert_eq!(cli["telemetry"]["enabled"].as_bool(), Some(false));
    assert!(home.join(".kiro/agents/tracedecay.json").exists());
}

#[test]
fn test_install_replaces_builtin_default_agent_choice() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    let cli_path = home.join(".kiro/settings/cli.json");
    std::fs::create_dir_all(cli_path.parent().unwrap()).unwrap();
    std::fs::write(&cli_path, r#"{"chat":{"defaultAgent":"kiro_default"}}"#).unwrap();

    KiroIntegration.install(&ctx).unwrap();

    let cli = read_json(&cli_path);
    assert_eq!(cli["chat"]["defaultAgent"].as_str(), Some("tracedecay"));
}

#[test]
fn test_has_tracedecay_tracks_global_mcp_entry() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    assert!(!KiroIntegration.has_tracedecay(home));

    KiroIntegration.install(&ctx).unwrap();
    assert!(KiroIntegration.has_tracedecay(home));

    KiroIntegration.uninstall(&ctx).unwrap();
    assert!(!KiroIntegration.has_tracedecay(home));
}

#[test]
fn test_healthcheck_clean_install_has_no_issues_or_warnings() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    KiroIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    KiroIntegration.healthcheck(&mut dc, &hctx);

    assert_eq!(dc.issues, 0, "clean Kiro install should have no issues");
    assert_eq!(dc.warnings, 0, "clean Kiro install should have no warnings");
}

#[test]
fn test_healthcheck_fails_when_steering_lacks_owned_end_marker() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    KiroIntegration.install(&ctx).unwrap();

    let steering_path = home.join(".kiro/steering/tracedecay.md");
    std::fs::write(
        &steering_path,
        "## Prefer tracedecay MCP tools\n\nEdited tracedecay guidance without ownership marker.\n",
    )
    .unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    KiroIntegration.healthcheck(&mut dc, &hctx);

    assert!(
        dc.issues > 0,
        "Kiro doctor should fail when tracedecay.md has tracedecay rules that install/uninstall cannot own"
    );
}

#[test]
fn test_healthcheck_warns_when_agent_tool_policy_is_not_permissive() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_ctx(home);

    KiroIntegration.install(&ctx).unwrap();

    let agent_path = home.join(".kiro/agents/tracedecay.json");
    let mut agent = read_json(&agent_path);
    agent["tools"] = serde_json::json!(["@tracedecay"]);
    agent["allowedTools"] = serde_json::json!(["@builtin"]);
    std::fs::write(&agent_path, serde_json::to_string_pretty(&agent).unwrap()).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    KiroIntegration.healthcheck(&mut dc, &hctx);

    assert_eq!(
        dc.issues, 0,
        "Kiro doctor should treat restrictive tool policy as drift, not broken MCP setup"
    );
    assert!(
        dc.warnings >= 2,
        "Kiro doctor should warn about both tools and allowedTools drift"
    );
}

#[test]
fn test_healthcheck_fails_when_workspace_mcp_disables_tracedecay() {
    let home_dir = TempDir::new().unwrap();
    let project_dir = TempDir::new().unwrap();
    let home = home_dir.path();
    let project = project_dir.path();
    let ctx = make_ctx(home);

    KiroIntegration.install(&ctx).unwrap();

    let workspace_mcp_path = project.join(".kiro/settings/mcp.json");
    std::fs::create_dir_all(workspace_mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &workspace_mcp_path,
        r#"{"mcpServers":{"tracedecay":{"command":"/usr/local/bin/tracedecay","args":["serve"],"disabled":true}}}"#,
    )
    .unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: project.to_path_buf(),
    };
    KiroIntegration.healthcheck(&mut dc, &hctx);

    assert!(
        dc.issues > 0,
        "workspace Kiro MCP override that disables tracedecay should be unhealthy"
    );
}

#[test]
fn test_healthcheck_fails_when_workspace_mcp_shadows_global_command() {
    let home_dir = TempDir::new().unwrap();
    let project_dir = TempDir::new().unwrap();
    let home = home_dir.path();
    let project = project_dir.path();
    let ctx = make_ctx(home);

    KiroIntegration.install(&ctx).unwrap();

    let workspace_mcp_path = project.join(".kiro/settings/mcp.json");
    std::fs::create_dir_all(workspace_mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &workspace_mcp_path,
        r#"{"mcpServers":{"tracedecay":{"command":"other-tracedecay","args":["serve"],"disabled":false}}}"#,
    )
    .unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: project.to_path_buf(),
    };
    KiroIntegration.healthcheck(&mut dc, &hctx);

    assert!(
        dc.issues > 0,
        "workspace Kiro MCP override with a different command should be unhealthy"
    );
}

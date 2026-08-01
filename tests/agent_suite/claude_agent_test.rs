use std::path::Path;

use tempfile::TempDir;
use tracedecay::agents::{
    AgentIntegration, ClaudeIntegration, DoctorCounters, HealthcheckContext, InstallContext,
    expected_tool_perms, tool_names,
};

use crate::agent_test_support::{install_ctx, install_ctx_with_real_bin};

/// Prefix for the plugin-namespace tool permission entries the installer writes
/// so the plugin MCP server's tools are auto-approved.
const PLUGIN_PERM_PREFIX: &str = "mcp__plugin_tracedecay_graph__";

/// Every managed tool's expected plugin-namespace permission entry.
fn expected_plugin_tool_perms() -> Vec<String> {
    tool_names()
        .into_iter()
        .map(|name| format!("{PLUGIN_PERM_PREFIX}{name}"))
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The Claude modules install with the dashboard deploy enabled; Claude
/// ignores the flag, so it costs nothing here and keeps the installed shape
/// closest to a real user install.
fn make_install_ctx(home: &Path) -> InstallContext {
    install_ctx(home, true)
}

/// Creates a fake tracedecay binary in a temp dir so healthcheck binary-exists
/// checks pass.
fn make_install_ctx_with_real_bin(home: &Path) -> InstallContext {
    install_ctx_with_real_bin(home, true)
}

fn read_json(path: &Path) -> serde_json::Value {
    let contents = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&contents).unwrap()
}

fn permission_allowlist(settings: &serde_json::Value) -> Vec<&str> {
    settings["permissions"]["allow"]
        .as_array()
        .expect("permissions.allow should be an array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect()
}

// ===========================================================================
// Install content verification
// ===========================================================================

#[test]
fn test_install_deploys_plugin_mcp_server() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    // The MCP server now lives in the deployed plugin's .mcp.json (rendered with
    // the resolved absolute binary path), not in ~/.claude.json.
    let plugin_mcp = home.join(".claude/plugins/marketplaces/tracedecay/.mcp.json");
    let mcp = read_json(&plugin_mcp);
    let ts = &mcp["mcpServers"]["graph"];
    assert!(ts.is_object(), "mcpServers.graph should be an object");
    assert_eq!(
        ts["command"].as_str().unwrap(),
        "/usr/local/bin/tracedecay",
        "command should be rendered with the resolved bin path"
    );
    let args: Vec<&str> = ts["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(args, vec!["serve"], "args should be [\"serve\"]");
}

#[test]
fn test_install_deploys_plugin_hooks() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    // Hooks are now provided by the plugin's own hooks/hooks.json (deployed with
    // the __TRACEDECAY_BIN__ placeholder rendered), not written into
    // ~/.claude/settings.json.
    let hooks_json =
        read_json(&home.join(".claude/plugins/marketplaces/tracedecay/hooks/hooks.json"));
    let hooks = hooks_json["hooks"]["PreToolUse"]
        .as_array()
        .expect("PreToolUse should be an array");

    let tracedecay_hook = hooks.iter().find(|h| {
        h.get("matcher").and_then(|m| m.as_str()) == Some("Agent")
            && h.get("hooks")
                .and_then(|a| a.as_array())
                .is_some_and(|arr| {
                    arr.iter().any(|entry| {
                        entry
                            .get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.contains("tracedecay"))
                    })
                })
    });
    assert!(
        tracedecay_hook.is_some(),
        "plugin PreToolUse should contain a hook with matcher=Agent and command containing tracedecay"
    );

    // Verify the hook command format (issue #81: modern args[] shape) and that
    // the binary placeholder was rendered to the resolved bin path.
    let hook = tracedecay_hook.unwrap();
    let inner = &hook["hooks"][0];
    let cmd = inner["command"].as_str().unwrap();
    assert_eq!(
        cmd, "/usr/local/bin/tracedecay",
        "hook command should be the rendered tracedecay exe path, got: {cmd}"
    );
    let args: Vec<&str> = inner["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        args,
        vec!["hook-pre-tool-use"],
        "subcommand must live in args[], not concatenated into command"
    );

    // The old config-managed settings.json must not carry a tracedecay hook.
    let settings = read_json(&home.join(".claude/settings.json"));
    assert!(
        settings.get("hooks").is_none(),
        "install must not write tracedecay hooks into settings.json (plugin provides them)"
    );
}

#[test]
fn test_install_creates_settings_with_permissions() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let settings = read_json(&home.join(".claude/settings.json"));
    let allow = settings["permissions"]["allow"]
        .as_array()
        .expect("permissions.allow should be an array");
    let allow_strs: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();

    for perm in expected_tool_perms() {
        assert!(
            allow_strs.contains(&perm.as_str()),
            "permissions.allow should contain {perm}"
        );
    }
}

#[test]
fn test_install_writes_plugin_namespace_permissions() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let settings = read_json(&home.join(".claude/settings.json"));
    let allow_strs = permission_allowlist(&settings);

    let plugin_perms = expected_plugin_tool_perms();
    assert!(
        !plugin_perms.is_empty(),
        "there should be at least one managed tool"
    );
    for perm in &plugin_perms {
        assert!(
            allow_strs.contains(&perm.as_str()),
            "permissions.allow should contain plugin-namespace entry {perm}"
        );
    }
}

#[test]
fn test_install_migrates_legacy_permissions_to_plugin_twins() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "permissions": { "allow": ["mcp__tracedecay__search", "Bash(*)"] }
        }))
        .unwrap(),
    )
    .unwrap();

    let mut ctx = make_install_ctx(home);
    ctx.tool_permissions = Vec::new();
    ClaudeIntegration.install(&ctx).unwrap();

    let settings = read_json(&claude_dir.join("settings.json"));
    let allow_strs = permission_allowlist(&settings);

    assert!(
        allow_strs.contains(&"mcp__tracedecay__search"),
        "legacy entry must be preserved (not removed)"
    );
    let plugin_search = format!("{PLUGIN_PERM_PREFIX}search");
    assert!(
        allow_strs.contains(&plugin_search.as_str()),
        "legacy mcp__tracedecay__search must gain its plugin-namespace twin"
    );
    assert!(
        allow_strs.contains(&"Bash(*)"),
        "unrelated permission must be preserved"
    );
}

/// A machine installed before the plugin MCP server key was renamed from
/// `tracedecay` to `graph` carries `mcp__plugin_tracedecay_tracedecay__*`
/// entries. Install must add the current `mcp__plugin_tracedecay_graph__*`
/// twin (so the renamed plugin server's tools stay auto-approved) while
/// preserving the prior entry.
#[test]
fn test_install_migrates_prior_plugin_permissions_to_graph_twins() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "permissions": { "allow": [
                "mcp__plugin_tracedecay_tracedecay__tracedecay_context",
                "Bash(*)"
            ] }
        }))
        .unwrap(),
    )
    .unwrap();

    let mut ctx = make_install_ctx(home);
    ctx.tool_permissions = Vec::new();
    ClaudeIntegration.install(&ctx).unwrap();

    let settings = read_json(&claude_dir.join("settings.json"));
    let allow_strs = permission_allowlist(&settings);

    assert!(
        allow_strs.contains(&"mcp__plugin_tracedecay_tracedecay__tracedecay_context"),
        "prior plugin-namespace entry must be preserved (not removed)"
    );
    let graph_twin = format!("{PLUGIN_PERM_PREFIX}tracedecay_context");
    assert!(
        allow_strs.contains(&graph_twin.as_str()),
        "prior mcp__plugin_tracedecay_tracedecay__* entry must gain its graph twin"
    );
    assert!(
        allow_strs.contains(&"Bash(*)"),
        "unrelated permission must be preserved"
    );
}

#[test]
fn test_install_claude_md_has_moment_triggers() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let claude_md = std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();
    assert!(
        claude_md.contains("Before your FIRST"),
        "CLAUDE.md should lead with the first-Grep moment trigger"
    );
    assert!(
        claude_md.contains("ToolSearch") && claude_md.contains("deferred"),
        "CLAUDE.md should note tools may be deferred and loadable via ToolSearch"
    );
    assert!(
        claude_md.contains("tracedecay_grep"),
        "CLAUDE.md should route literal/regex content search to tracedecay_grep"
    );
    assert!(
        claude_md.contains("tracedecay_search"),
        "CLAUDE.md should route symbol-by-name search to tracedecay_search"
    );
    assert!(
        claude_md.contains("subagents") || claude_md.contains("subagent"),
        "CLAUDE.md should note the block reaches subagents"
    );
}

#[test]
fn test_install_creates_claude_md_with_rules() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let claude_md = std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();
    assert!(
        claude_md.contains("## MANDATORY: No Explore Agents When Tracedecay Is Available"),
        "CLAUDE.md should contain the mandatory rules marker"
    );
    assert!(
        claude_md.contains("tracedecay_context"),
        "CLAUDE.md should mention tracedecay tools"
    );
    assert!(
        claude_md.contains("NEVER use Agent(subagent_type=Explore)"),
        "CLAUDE.md should contain the no-explore-agent rule"
    );
    assert!(
        claude_md.contains("When you spawn an Explore agent"),
        "CLAUDE.md should contain the explore agent guidance paragraph"
    );
    assert!(
        claude_md.contains("exclude_node_ids"),
        "CLAUDE.md should mention exclude_node_ids for dedup"
    );
}

#[test]
fn test_claude_md_contains_explore_agent_paragraph() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);

    // Pre-populate CLAUDE.md with existing content
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(claude_dir.join("CLAUDE.md"), "# Existing content\n").unwrap();

    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let content = std::fs::read_to_string(claude_dir.join("CLAUDE.md")).unwrap();
    assert!(
        content.contains("When you spawn an Explore agent"),
        "should contain explore agent paragraph"
    );
    assert!(
        content.contains("tracedecay_context"),
        "should reference tracedecay_context as the tool"
    );
    assert!(
        content.contains("exclude_node_ids"),
        "should mention exclude_node_ids for dedup"
    );
}

#[test]
fn test_uninstall_removes_explore_agent_paragraph() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);

    // Pre-populate CLAUDE.md with existing content
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("CLAUDE.md"),
        "# My Rules\n\nKeep it clean.\n",
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    // Verify install added the explore agent paragraph
    let content = std::fs::read_to_string(claude_dir.join("CLAUDE.md")).unwrap();
    assert!(content.contains("When you spawn an Explore agent"));

    // Now uninstall
    ClaudeIntegration.uninstall(&ctx).unwrap();

    let content = std::fs::read_to_string(claude_dir.join("CLAUDE.md")).unwrap();
    assert!(
        !content.contains("When you spawn an Explore agent"),
        "explore agent paragraph should be removed after uninstall"
    );
    assert!(
        content.contains("My Rules"),
        "existing content should be preserved after uninstall"
    );
}

#[test]
fn test_install_idempotent_claude_md() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    ClaudeIntegration.install(&ctx).unwrap();
    ClaudeIntegration.install(&ctx).unwrap();

    let claude_md = std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();
    let marker = "## MANDATORY: No Explore Agents When Tracedecay Is Available";
    let count = claude_md.matches(marker).count();
    assert_eq!(
        count, 1,
        "marker should appear exactly once after double install, found {count}"
    );
}

#[test]
fn test_install_preserves_existing_claude_json() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);

    // Pre-populate .claude.json with an extra key
    std::fs::write(home.join(".claude.json"), r#"{"foo": "bar"}"#).unwrap();

    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let claude_json = read_json(&home.join(".claude.json"));
    assert_eq!(
        claude_json["foo"].as_str().unwrap(),
        "bar",
        "existing key 'foo' should be preserved"
    );
    // The plugin model no longer writes tracedecay into ~/.claude.json.
    assert!(
        claude_json
            .get("mcpServers")
            .and_then(|v| v.get("tracedecay"))
            .is_none(),
        "install must not add a config-managed MCP entry to ~/.claude.json"
    );
}

#[test]
fn test_install_preserves_existing_settings() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);

    // Pre-populate settings.json with an existing hook
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [{"type": "command", "command": "echo hello"}]
      }
    ]
  }
}"#,
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let settings = read_json(&claude_dir.join("settings.json"));
    let hooks = settings["hooks"]["PreToolUse"].as_array().unwrap();

    // The user's existing (non-tracedecay) Bash hook must be preserved, and the
    // plugin must be enabled. Install no longer injects a tracedecay Agent hook
    // into settings.json — the plugin's own hooks.json provides it.
    let has_bash = hooks
        .iter()
        .any(|h| h.get("matcher").and_then(|m| m.as_str()) == Some("Bash"));
    assert!(has_bash, "existing Bash hook should be preserved");
    let has_tracedecay_hook = hooks.iter().any(|h| {
        h.get("hooks")
            .and_then(|a| a.as_array())
            .is_some_and(|arr| {
                arr.iter().any(|entry| {
                    entry
                        .get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c.contains("tracedecay"))
                })
            })
    });
    assert!(
        !has_tracedecay_hook,
        "install must not add a tracedecay hook to settings.json (plugin provides it)"
    );
    assert_eq!(
        settings["enabledPlugins"]["tracedecay@tracedecay"],
        serde_json::json!(true),
        "install should enable the plugin"
    );
}

#[test]
fn test_install_migrates_off_config_managed_integration() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    // Seed a legacy config-managed install: loose MCP entry in ~/.claude.json,
    // a tracedecay hook in settings.json, and a loose managed subagent file.
    std::fs::write(
        home.join(".claude.json"),
        r#"{
  "mcpServers": {
    "tracedecay": { "command": "/old/path/tracedecay", "args": ["serve"] },
    "other": { "command": "keep-me" }
  }
}"#,
    )
    .unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Agent",
        "hooks": [{"type": "command", "command": "/old/path/tracedecay hook-pre-tool-use"}]
      }
    ]
  }
}"#,
    )
    .unwrap();
    let agents_dir = claude_dir.join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("code-explorer.md"),
        "---\nname: code-explorer\n---\nUse tracedecay for exploration.\n",
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    // The loose MCP tracedecay entry is migrated away; the foreign server stays.
    let claude_json = read_json(&home.join(".claude.json"));
    assert!(
        claude_json
            .get("mcpServers")
            .and_then(|v| v.get("tracedecay"))
            .is_none(),
        "legacy loose MCP tracedecay entry should be migrated away"
    );
    assert!(
        claude_json["mcpServers"]["other"].is_object(),
        "foreign MCP server must be preserved during migration"
    );

    // The tracedecay hook is migrated out of settings.json.
    let settings = read_json(&claude_dir.join("settings.json"));
    let has_tracedecay_hook = settings
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|v| v.as_array())
        .is_some_and(|arr| {
            arr.iter().any(|w| {
                w.get("hooks")
                    .and_then(|a| a.as_array())
                    .is_some_and(|inner| {
                        inner.iter().any(|e| {
                            e.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|c| c.contains("tracedecay"))
                        })
                    })
            })
        });
    assert!(
        !has_tracedecay_hook,
        "legacy tracedecay hook should be migrated out of settings.json"
    );

    // The loose managed subagent is removed.
    assert!(
        !agents_dir.join("code-explorer.md").exists(),
        "loose tracedecay-managed subagent should be migrated away"
    );
}

// ===========================================================================
// Uninstall content verification
// ===========================================================================

#[test]
fn test_uninstall_removes_mcp_from_claude_json() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();
    ClaudeIntegration.uninstall(&ctx).unwrap();

    // File may be deleted (empty) or exist without tracedecay
    let path = home.join(".claude.json");
    if path.exists() {
        let claude_json = read_json(&path);
        let has_tracedecay = claude_json
            .get("mcpServers")
            .and_then(|v| v.get("tracedecay"))
            .is_some();
        assert!(
            !has_tracedecay,
            "mcpServers.tracedecay should be gone after uninstall"
        );
    }
}

#[test]
fn test_uninstall_removes_deployed_bundle_and_lone_marketplace_file() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    // Install deploys the plugin bundle and registers the marketplace.
    ClaudeIntegration.install(&ctx).unwrap();
    let deploy_dir = home.join(".claude/plugins/marketplaces/tracedecay");
    let known_path = home.join(".claude/plugins/known_marketplaces.json");
    assert!(
        deploy_dir.exists(),
        "bundle should be deployed after install"
    );
    assert!(
        known_path.exists(),
        "known_marketplaces.json should exist after install"
    );
    // Claude Code's marketplace schema requires these fields; without them
    // `claude plugin install` rejects the entry as corrupted and the plugin
    // silently never loads.
    let known: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&known_path).unwrap()).unwrap();
    let entry = &known["tracedecay"];
    assert_eq!(
        entry["installLocation"].as_str(),
        Some(deploy_dir.to_string_lossy().as_ref()),
        "marketplace entry must carry installLocation"
    );
    assert!(
        entry["lastUpdated"]
            .as_str()
            .is_some_and(|ts| ts.ends_with('Z')),
        "marketplace entry must carry an ISO-8601 lastUpdated: {entry}"
    );

    ClaudeIntegration.uninstall(&ctx).unwrap();

    // The deployed bundle dir is removed entirely.
    assert!(
        !deploy_dir.exists(),
        "deployed plugin bundle should be removed after uninstall"
    );
    // known_marketplaces.json held only tracedecay, so it should be deleted.
    assert!(
        !known_path.exists(),
        "known_marketplaces.json should be deleted when tracedecay was its only entry"
    );
}

#[test]
fn test_uninstall_removes_hook_from_settings() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();
    ClaudeIntegration.uninstall(&ctx).unwrap();

    let settings_path = home.join(".claude/settings.json");
    if settings_path.exists() {
        let settings = read_json(&settings_path);
        let has_hook = settings["hooks"]["PreToolUse"]
            .as_array()
            .is_some_and(|arr| {
                arr.iter().any(|h| {
                    h.get("hooks")
                        .and_then(|a| a.as_array())
                        .is_some_and(|arr| {
                            arr.iter().any(|entry| {
                                entry
                                    .get("command")
                                    .and_then(|c| c.as_str())
                                    .is_some_and(|c| c.contains("tracedecay"))
                            })
                        })
                })
            });
        assert!(
            !has_hook,
            "PreToolUse should not contain tracedecay hook after uninstall"
        );
    }
}

#[test]
fn test_uninstall_removes_permissions_from_settings() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();
    ClaudeIntegration.uninstall(&ctx).unwrap();

    let settings_path = home.join(".claude/settings.json");
    if settings_path.exists() {
        let settings = read_json(&settings_path);
        let has_ts_perm = settings["permissions"]["allow"]
            .as_array()
            .is_some_and(|arr| {
                arr.iter().any(|v| {
                    v.as_str()
                        .is_some_and(|s| s.starts_with("mcp__tracedecay__"))
                })
            });
        assert!(
            !has_ts_perm,
            "permissions.allow should not contain mcp__tracedecay__* after uninstall"
        );
    }
}

#[test]
fn test_uninstall_preserves_other_permissions() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);

    // Install first so all files are set up
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    // Now add a non-tracedecay permission to settings.json
    let settings_path = home.join(".claude/settings.json");
    let mut settings = read_json(&settings_path);
    let allow = settings["permissions"]["allow"].as_array_mut().unwrap();
    allow.push(serde_json::json!("Bash(*)"));
    let pretty = serde_json::to_string_pretty(&settings).unwrap();
    std::fs::write(&settings_path, format!("{pretty}\n")).unwrap();

    ClaudeIntegration.uninstall(&ctx).unwrap();

    let settings = read_json(&settings_path);
    let allow = settings["permissions"]["allow"]
        .as_array()
        .expect("permissions.allow should still exist");
    let strs: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        strs.contains(&"Bash(*)"),
        "non-tracedecay permission 'Bash(*)' should be preserved, got: {strs:?}"
    );
    assert!(
        !strs.iter().any(|s| s.starts_with("mcp__tracedecay__")),
        "tracedecay permissions should be removed"
    );
}

#[test]
fn test_uninstall_removes_claude_md_rules() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let claude_md_path = home.join(".claude/CLAUDE.md");
    assert!(claude_md_path.exists());

    ClaudeIntegration.uninstall(&ctx).unwrap();

    // CLAUDE.md had only tracedecay rules, should be removed
    if claude_md_path.exists() {
        let content = std::fs::read_to_string(&claude_md_path).unwrap();
        assert!(
            !content.contains("MANDATORY: No Explore Agents When Tracedecay Is Available"),
            "CLAUDE.md should not contain tracedecay marker after uninstall"
        );
    }
}

#[test]
fn test_uninstall_preserves_other_claude_md_content() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);

    // Create CLAUDE.md with pre-existing content
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("CLAUDE.md"),
        "## My Custom Rules\n\nAlways write tests.\n",
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    // Verify install appended rules
    let md_content = std::fs::read_to_string(claude_dir.join("CLAUDE.md")).unwrap();
    assert!(md_content.contains("My Custom Rules"));
    assert!(md_content.contains("MANDATORY: No Explore Agents"));

    ClaudeIntegration.uninstall(&ctx).unwrap();

    // After uninstall, custom content should remain
    let md_content = std::fs::read_to_string(claude_dir.join("CLAUDE.md")).unwrap();
    assert!(
        md_content.contains("My Custom Rules"),
        "custom content should be preserved after uninstall"
    );
    assert!(
        md_content.contains("Always write tests"),
        "custom content body should be preserved"
    );
    assert!(
        !md_content.contains("MANDATORY: No Explore Agents"),
        "tracedecay marker should be removed after uninstall"
    );
}

// ===========================================================================
// Healthcheck verification
// ===========================================================================

#[test]
fn test_healthcheck_detects_missing_plugin() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);
    // With nothing installed, the plugin manifest is absent — the doctor flags
    // it (as a warning to install the plugin) plus missing CLAUDE.md.
    assert!(
        dc.issues > 0 || dc.warnings > 0,
        "healthcheck should detect the missing plugin bundle"
    );
}

#[test]
fn test_healthcheck_detects_missing_settings() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Create .claude.json with MCP server but no settings.json
    std::fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers":{"tracedecay":{"command":"/usr/local/bin/tracedecay","args":["serve"]}}}"#,
    )
    .unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);

    // Should detect missing settings.json (hooks/permissions) and missing CLAUDE.md
    assert!(
        dc.issues > 0 || dc.warnings > 0,
        "healthcheck should detect missing settings.json"
    );
}

#[test]
fn test_healthcheck_detects_missing_permissions() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Create .claude.json with MCP server
    std::fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers":{"tracedecay":{"command":"/usr/local/bin/tracedecay","args":["serve"]}}}"#,
    )
    .unwrap();

    // Create settings.json with hook but NO permissions
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Agent",
        "hooks": [{"type": "command", "command": "/usr/local/bin/tracedecay hook-pre-tool-use"}]
      }
    ]
  }
}"#,
    )
    .unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);
    assert!(
        dc.issues > 0,
        "healthcheck should detect missing permissions"
    );
}

#[test]
fn test_healthcheck_detects_stale_permissions() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx_with_real_bin(home);
    ClaudeIntegration.install(&ctx).unwrap();

    // Add a stale permission that is not in EXPECTED_TOOL_PERMS
    let settings_path = home.join(".claude/settings.json");
    let mut settings = read_json(&settings_path);
    let allow = settings["permissions"]["allow"].as_array_mut().unwrap();
    allow.push(serde_json::json!("mcp__tracedecay__fake_tool"));
    let pretty = serde_json::to_string_pretty(&settings).unwrap();
    std::fs::write(&settings_path, format!("{pretty}\n")).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);
    assert!(
        dc.warnings > 0,
        "healthcheck should warn about stale permissions"
    );
}

#[test]
fn test_healthcheck_detects_missing_claude_md() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx_with_real_bin(home);
    ClaudeIntegration.install(&ctx).unwrap();

    // Delete CLAUDE.md
    std::fs::remove_file(home.join(".claude/CLAUDE.md")).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);
    assert!(
        dc.warnings > 0,
        "healthcheck should warn about missing CLAUDE.md"
    );
}

#[test]
fn test_healthcheck_clean_local_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let project = dir.path().join("myproject");
    std::fs::create_dir_all(&project).unwrap();

    // Create a local .mcp.json with tracedecay
    std::fs::write(
        project.join(".mcp.json"),
        r#"{"mcpServers":{"tracedecay":{"command":"/usr/local/bin/tracedecay","args":["serve"]}}}"#,
    )
    .unwrap();

    // Install in home so healthcheck doesn't fail on missing global config
    let ctx = make_install_ctx_with_real_bin(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: project.clone(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);

    // The local .mcp.json should be cleaned up (removed entirely since tracedecay
    // was the only entry)
    assert!(
        !project.join(".mcp.json").exists(),
        "local .mcp.json should be removed after healthcheck cleanup"
    );
}

#[test]
fn test_healthcheck_local_settings_cleanup() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let project = dir.path().join("myproject");
    let local_claude = project.join(".claude");
    std::fs::create_dir_all(&local_claude).unwrap();

    // Create local settings.local.json with tracedecay entries
    std::fs::write(
        local_claude.join("settings.local.json"),
        r#"{
  "enableAllProjectMcpServers": false,
  "enabledMcpjsonServers": ["tracedecay"],
  "mcpServers": {
    "tracedecay": {
      "command": "/usr/local/bin/tracedecay",
      "args": ["serve"]
    }
  }
}"#,
    )
    .unwrap();

    // Install in home so healthcheck doesn't fail on missing global config
    let ctx = make_install_ctx_with_real_bin(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: project.clone(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);

    // The local settings.local.json should be cleaned up
    // (removed entirely since tracedecay was the only content that mattered)
    assert!(
        !local_claude.join("settings.local.json").exists(),
        "settings.local.json should be removed after healthcheck cleanup"
    );
}

// ===========================================================================
// is_detected / has_tracedecay
// ===========================================================================

#[test]
fn test_has_tracedecay_after_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    assert!(
        ClaudeIntegration.has_tracedecay(home),
        "has_tracedecay should return true after install"
    );
}

#[test]
fn test_has_tracedecay_without_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    assert!(
        !ClaudeIntegration.has_tracedecay(home),
        "has_tracedecay should return false without install"
    );
}

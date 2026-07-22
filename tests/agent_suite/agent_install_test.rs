//! Cross-agent install/uninstall tests: config creation, symlink
//! containment, project-path writes, backup/preserve behavior, and
//! idempotency.

use crate::agent_test_support::*;
use crate::common::{EnvVarGuard, PROCESS_ENV_LOCK as AGENT_ENV_LOCK};
use tempfile::TempDir;
use tracedecay::agents::*;
use tracedecay::config::USER_DATA_DIR_ENV;

#[cfg(unix)]
#[test]
fn test_local_install_claude_rejects_symlinked_claude_dir() {
    assert_local_install_rejects_symlinked_target("claude", ".claude", true);
}

#[cfg(unix)]
#[test]
fn test_local_install_copilot_rejects_symlinked_vscode_dir() {
    assert_local_install_rejects_symlinked_target("copilot", ".vscode", true);
}

#[cfg(unix)]
#[test]
fn test_local_install_gemini_rejects_symlinked_gemini_dir() {
    assert_local_install_rejects_symlinked_target("gemini", ".gemini", true);
}

#[cfg(unix)]
#[test]
fn test_hermes_local_install_is_unsupported() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let output = tracedecay_command(project.path(), home.path())
        .args(["install", "--local", "--agent", "hermes"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not support"));
    assert!(!project.path().join(".hermes").exists());
}

#[cfg(unix)]
#[test]
fn test_local_install_kilo_rejects_symlinked_config_file() {
    assert_local_install_rejects_symlinked_target("kilo", "kilo.json", false);
}

#[cfg(unix)]
#[test]
fn test_local_install_kimi_rejects_symlinked_kimi_dir() {
    assert_local_install_rejects_symlinked_target("kimi", ".kimi-code", true);
}

#[cfg(unix)]
#[test]
fn test_local_install_kiro_rejects_symlinked_kiro_dir() {
    assert_local_install_rejects_symlinked_target("kiro", ".kiro", true);
}

#[cfg(unix)]
#[test]
fn test_local_install_opencode_rejects_symlinked_config_file() {
    assert_local_install_rejects_symlinked_target("opencode", "opencode.json", false);
}

#[cfg(unix)]
#[test]
fn test_local_install_roo_code_rejects_symlinked_roo_dir() {
    assert_local_install_rejects_symlinked_target("roo-code", ".roo", true);
}

#[cfg(unix)]
#[test]
fn test_local_install_vibe_rejects_symlinked_vibe_dir() {
    assert_local_install_rejects_symlinked_target("vibe", ".vibe", true);
}

#[cfg(unix)]
#[test]
fn test_local_install_zed_rejects_symlinked_zed_dir() {
    assert_local_install_rejects_symlinked_target("zed", ".zed", true);
}

#[test]
fn test_local_install_claude_writes_project_paths() {
    // Claude Code plugins are global (deployed under ~/.claude/plugins), so a
    // `--local` install ensures the global plugin is present and only writes
    // the genuinely project-scoped part: the CLAUDE.md steering rules. It does
    // not write a project `.mcp.json` or `.claude/settings.json`.
    assert_local_install_writes_project_paths("claude", &[".claude/CLAUDE.md"]);
}

#[test]
fn test_local_install_codex_writes_project_paths() {
    assert_local_install_writes_project_paths(
        "codex",
        &[
            ".agents/plugins/marketplace.json",
            "plugins/tracedecay/.codex-plugin/plugin.json",
            "plugins/tracedecay/.mcp.json",
            "plugins/tracedecay/skills/exploring-code/SKILL.md",
        ],
    );
}

#[test]
fn test_local_install_gemini_writes_project_paths() {
    assert_local_install_writes_project_paths("gemini", &[".gemini/settings.json", "GEMINI.md"]);
}

#[test]
fn test_local_install_kiro_writes_project_paths() {
    assert_local_install_writes_project_paths(
        "kiro",
        &[
            ".kiro/settings/mcp.json",
            ".kiro/steering/tracedecay.md",
            ".kiro/agents/tracedecay.json",
        ],
    );
}

#[test]
fn test_local_install_opencode_writes_project_paths() {
    assert_local_install_writes_project_paths("opencode", &["opencode.json", "AGENTS.md"]);
}

#[test]
fn test_local_install_copilot_writes_project_paths() {
    assert_local_install_writes_project_paths("copilot", &[".vscode/mcp.json"]);
}

#[test]
fn test_local_install_zed_writes_project_paths() {
    assert_local_install_writes_project_paths("zed", &[".zed/settings.json"]);
}

#[test]
fn test_local_install_roo_code_writes_project_paths() {
    assert_local_install_writes_project_paths("roo-code", &[".roo/mcp.json"]);
}

#[test]
fn test_local_install_kimi_writes_project_paths() {
    assert_local_install_writes_project_paths("kimi", &[".kimi-code/mcp.json", "AGENTS.md"]);
}

#[test]
fn test_local_install_kilo_writes_project_paths() {
    assert_local_install_writes_project_paths("kilo", &["kilo.json"]);
}

#[test]
fn test_local_install_vibe_writes_project_paths() {
    assert_local_install_writes_project_paths(
        "vibe",
        &[".vibe/config.toml", ".vibe/prompts/cli.md"],
    );
}

#[test]
fn test_local_install_rejects_antigravity_without_project_mutation() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let output = run_local_install("antigravity", project.path(), home.path());

    assert!(
        !output.status.success(),
        "Antigravity local install should be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Antigravity") && stderr.contains("--local"),
        "unsupported-agent error should name Antigravity and --local, got:\n{stderr}"
    );
    assert!(
        !home.path().join(".tracedecay/config.toml").exists(),
        "rejected local install must not mutate user-level install tracking"
    );
}

#[test]
fn test_local_install_rejects_cline_without_project_mutation() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let output = run_local_install("cline", project.path(), home.path());

    assert!(
        !output.status.success(),
        "Cline local install should be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Cline") && stderr.contains("--local"),
        "unsupported-agent error should name Cline and --local, got:\n{stderr}"
    );
    assert!(
        !project.path().join(".cline_mcp_servers.json").exists(),
        "unsupported Cline local install must not write undocumented workspace config"
    );
    assert!(
        !home.path().join(".tracedecay/config.toml").exists(),
        "rejected local install must not mutate user-level install tracking"
    );
}

#[test]
fn test_claude_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    // The plugin bundle is deployed to the stable marketplace dir; the MCP
    // server now lives in the plugin's own .mcp.json (not ~/.claude.json).
    let marketplace_manifest =
        home.join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/marketplace.json");
    assert!(
        marketplace_manifest.exists(),
        "plugin marketplace manifest should be deployed after install"
    );
    let plugin_mcp = home.join(".claude/plugins/marketplaces/tracedecay/.mcp.json");
    assert!(plugin_mcp.exists(), "plugin .mcp.json should be deployed");
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&plugin_mcp).unwrap()).unwrap();
    assert!(
        mcp["mcpServers"]["graph"].is_object(),
        "plugin .mcp.json should define the graph MCP server"
    );

    // The marketplace is registered in known_marketplaces.json as a directory.
    let known: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".claude/plugins/known_marketplaces.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        known["tracedecay"]["source"]["source"].as_str(),
        Some("directory"),
        "known_marketplaces.json should register tracedecay as a directory marketplace"
    );

    // settings.json enables the plugin and carries the MCP tool permissions.
    let settings_path = home.join(".claude/settings.json");
    assert!(
        settings_path.exists(),
        "settings.json should exist after install"
    );
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(
        settings["enabledPlugins"]["tracedecay@tracedecay"],
        serde_json::json!(true),
        "settings.json should enable the tracedecay plugin"
    );
    // Check permissions
    assert!(
        settings["permissions"]["allow"].is_array(),
        "permissions.allow should be an array"
    );
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    let allow_strs: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();
    for perm in expected_tool_perms() {
        assert!(
            allow_strs.contains(&perm.as_str()),
            "permissions.allow should contain {perm}"
        );
    }

    // The old config-managed ~/.claude.json MCP entry must NOT be written.
    let claude_json = home.join(".claude.json");
    if claude_json.exists() {
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        assert!(
            content
                .get("mcpServers")
                .and_then(|v| v.get("tracedecay"))
                .is_none(),
            "install must not write the legacy config-managed MCP entry to ~/.claude.json"
        );
    }

    // Check CLAUDE.md exists with tracedecay rules
    let claude_md = home.join(".claude/CLAUDE.md");
    assert!(claude_md.exists(), "CLAUDE.md should exist after install");
    let md_content = std::fs::read_to_string(&claude_md).unwrap();
    assert!(
        md_content.contains("tracedecay"),
        "CLAUDE.md should mention tracedecay"
    );
}

#[test]
fn test_gemini_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    GeminiIntegration.install(&ctx).unwrap();

    // Check ~/.gemini/settings.json
    let settings_path = home.join(".gemini/settings.json");
    assert!(
        settings_path.exists(),
        "settings.json should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(
        content["mcpServers"]["tracedecay"].is_object(),
        "mcpServers.tracedecay should exist"
    );
    // Verify trust flag
    assert_eq!(
        content["mcpServers"]["tracedecay"]["trust"],
        serde_json::json!(true),
        "gemini should have trust: true"
    );

    // Check GEMINI.md
    let gemini_md = home.join(".gemini/GEMINI.md");
    assert!(gemini_md.exists(), "GEMINI.md should exist after install");
    let md_content = std::fs::read_to_string(&gemini_md).unwrap();
    assert!(md_content.contains("tracedecay"));
}

#[test]
fn test_kimi_uninstall_removes_legacy_global_install() {
    // Migration shim: tracedecay versions before the Kimi Code CLI plugin
    // became the global install wrote `~/.kimi/mcp.json` and
    // `~/.kimi/AGENTS.md`. Current installs never write them, but uninstall
    // must still clean up a pre-plugin install.
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let _kimi_code_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        home.join(".kimi-code"),
    );
    let ctx = make_install_ctx(home);

    let kimi_dir = home.join(".kimi");
    std::fs::create_dir_all(&kimi_dir).unwrap();
    let mcp_path = kimi_dir.join("mcp.json");
    std::fs::write(
        &mcp_path,
        "{\n  \"mcpServers\": {\n    \"tracedecay\": { \"command\": \"/old/tracedecay\", \"args\": [\"serve\"] }\n  }\n}\n",
    )
    .unwrap();
    let agents_md = kimi_dir.join("AGENTS.md");
    std::fs::write(
        &agents_md,
        "## Prefer tracedecay MCP tools\n\nUse tracedecay MCP tools first.\n",
    )
    .unwrap();
    assert!(
        KimiIntegration.has_tracedecay(home),
        "the legacy registration branch should still be noticed"
    );

    KimiIntegration.uninstall(&ctx).unwrap();

    assert!(
        !mcp_path.exists(),
        "legacy mcp.json with only tracedecay should be removed on uninstall"
    );
    assert!(
        !agents_md.exists(),
        "legacy AGENTS.md holding only tracedecay rules should be removed on uninstall"
    );
    assert!(!KimiIntegration.has_tracedecay(home));
}

#[test]
fn test_kimi_is_detected_and_has_tracedecay() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let _kimi_code_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        home.join(".kimi-code"),
    );

    assert!(!KimiIntegration.is_detected(home));
    assert!(!KimiIntegration.has_tracedecay(home));

    let ctx = make_install_ctx(home);
    KimiIntegration.install(&ctx).unwrap();

    assert!(KimiIntegration.is_detected(home));
    assert!(KimiIntegration.has_tracedecay(home));
}

// ---------------------------------------------------------------------------
// Kimi Code CLI native plugin
// ---------------------------------------------------------------------------

/// Reads `<kimi-code-home>/plugins/installed.json` and returns the tracedecay
/// entry (tests fail loudly when the registry or entry is missing).
fn kimi_installed_entry(kimi_code_home: &std::path::Path) -> serde_json::Value {
    let installed_path = kimi_code_home.join("plugins/installed.json");
    let installed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&installed_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", installed_path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", installed_path.display()));
    assert_eq!(installed["version"], serde_json::json!(1));
    installed["plugins"]
        .as_array()
        .unwrap_or_else(|| panic!("{}: plugins should be an array", installed_path.display()))
        .iter()
        .find(|entry| entry["id"] == "tracedecay")
        .cloned()
        .unwrap_or_else(|| panic!("{}: missing tracedecay entry", installed_path.display()))
}

#[test]
fn test_kimi_install_deploys_native_plugin() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let kimi_code_home = home.join("kimi-code-home");
    let _kimi_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        &kimi_code_home,
    );
    let ctx = make_install_ctx(home);
    KimiIntegration.install(&ctx).unwrap();

    // The plugin is the whole global install: no legacy `~/.kimi` surface.
    assert!(
        !home.join(".kimi").exists(),
        "install must not write the legacy ~/.kimi tree"
    );

    // Managed bundle: manifest, README, at least one skill and one command.
    let managed_dir = kimi_code_home.join("plugins/managed/tracedecay");
    let manifest_path = managed_dir.join(".kimi-plugin/plugin.json");
    assert!(manifest_path.exists(), "plugin manifest should be deployed");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["name"], "tracedecay");
    assert_eq!(
        manifest["version"],
        env!("CARGO_PKG_VERSION"),
        "manifest should be stamped with the crate version"
    );
    assert_eq!(
        manifest["mcpServers"]["tracedecay"]["command"], ctx.tracedecay_bin,
        "manifest MCP command should bake in the resolved tracedecay binary"
    );

    let mut deployed = Vec::new();
    collect_files_recursive(&managed_dir, &managed_dir, &mut deployed);
    let relatives: Vec<String> = deployed
        .iter()
        .map(|(relative, _)| relative.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(
        relatives.iter().any(|path| path == "README.md"),
        "bundle should ship README.md, got {relatives:?}"
    );
    assert!(
        relatives
            .iter()
            .any(|path| path.starts_with("skills/") && path.ends_with("SKILL.md")),
        "bundle should ship skills/<name>/SKILL.md entries, got {relatives:?}"
    );
    assert!(
        relatives
            .iter()
            .any(|path| path.starts_with("commands/") && path.ends_with(".md")),
        "bundle should ship commands/*.md entries, got {relatives:?}"
    );

    // Registry entry.
    let entry = kimi_installed_entry(&kimi_code_home);
    assert_eq!(entry["source"], "local-path");
    assert_eq!(entry["enabled"], serde_json::json!(true));
    assert!(
        entry["installedAt"]
            .as_str()
            .is_some_and(|ts| ts.ends_with('Z')),
        "installedAt should be an ISO-8601 UTC timestamp: {entry}"
    );
    assert_eq!(
        entry["installedAt"], entry["updatedAt"],
        "fresh install should set updatedAt to installedAt"
    );
    let root = entry["root"].as_str().expect("root should be a string");
    assert_eq!(
        std::path::Path::new(root),
        managed_dir.canonicalize().unwrap().as_path(),
        "root should be the canonical managed plugin dir"
    );
}

#[test]
fn test_kimi_reinstall_preserves_enabled_and_installed_at() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let kimi_code_home = home.join("kimi-code-home");
    let _kimi_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        &kimi_code_home,
    );
    let ctx = make_install_ctx(home);
    KimiIntegration.install(&ctx).unwrap();

    // Simulate a user-disabled, long-installed entry.
    let installed_path = kimi_code_home.join("plugins/installed.json");
    let mut installed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&installed_path).unwrap()).unwrap();
    let entry = installed["plugins"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["id"] == "tracedecay")
        .unwrap();
    entry["enabled"] = serde_json::json!(false);
    entry["installedAt"] = serde_json::json!("2020-01-01T00:00:00Z");
    std::fs::write(
        &installed_path,
        format!("{}\n", serde_json::to_string_pretty(&installed).unwrap()),
    )
    .unwrap();

    KimiIntegration.install(&ctx).unwrap();

    let entry = kimi_installed_entry(&kimi_code_home);
    assert_eq!(
        entry["enabled"],
        serde_json::json!(false),
        "reinstall must preserve a user-set enabled:false"
    );
    assert_eq!(
        entry["installedAt"],
        serde_json::json!("2020-01-01T00:00:00Z"),
        "reinstall must preserve the original installedAt"
    );
    let updated_at = entry["updatedAt"].as_str().unwrap();
    assert!(
        updated_at > "2020-01-01T00:00:00Z",
        "reinstall must bump updatedAt, got {updated_at}"
    );
}

#[test]
fn test_kimi_install_migrates_kimi_code_mcp_json() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let kimi_code_home = home.join("kimi-code-home");
    let _kimi_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        &kimi_code_home,
    );
    let mcp_path = kimi_code_home.join("mcp.json");
    std::fs::create_dir_all(&kimi_code_home).unwrap();
    std::fs::write(
        &mcp_path,
        "{\n  \"mcpServers\": {\n    \"tracedecay\": { \"command\": \"/old/tracedecay\", \"args\": [\"serve\"] },\n    \"other\": { \"command\": \"other-bin\" }\n  }\n}\n",
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    KimiIntegration.install(&ctx).unwrap();

    // The plugin now owns the MCP server: the direct registration is stripped,
    // foreign servers are kept, and a backup was taken first.
    let migrated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
    assert!(
        migrated["mcpServers"].get("tracedecay").is_none(),
        "install should strip mcpServers.tracedecay from <kimi-code-home>/mcp.json"
    );
    assert_eq!(migrated["mcpServers"]["other"]["command"], "other-bin");
    assert!(
        kimi_code_home.join("mcp.json.bak").exists(),
        "migration should leave a .bak backup"
    );

    // When tracedecay was the only entry, the file is deleted outright.
    std::fs::write(
        &mcp_path,
        "{\n  \"mcpServers\": {\n    \"tracedecay\": { \"command\": \"/old/tracedecay\", \"args\": [\"serve\"] }\n  }\n}\n",
    )
    .unwrap();
    KimiIntegration.install(&ctx).unwrap();
    assert!(
        !mcp_path.exists(),
        "mcp.json left empty by the migration should be deleted"
    );
}

#[test]
fn test_kimi_uninstall_removes_native_plugin() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let kimi_code_home = home.join("kimi-code-home");
    let _kimi_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        &kimi_code_home,
    );
    let ctx = make_install_ctx(home);
    KimiIntegration.install(&ctx).unwrap();

    let managed_dir = kimi_code_home.join("plugins/managed/tracedecay");
    assert!(managed_dir.exists());
    assert_eq!(kimi_installed_entry(&kimi_code_home)["id"], "tracedecay");

    KimiIntegration.uninstall(&ctx).unwrap();

    assert!(
        !managed_dir.exists(),
        "uninstall should remove the managed plugin dir"
    );
    let installed_path = kimi_code_home.join("plugins/installed.json");
    let installed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&installed_path).unwrap()).unwrap();
    assert!(
        installed["plugins"].as_array().unwrap().is_empty(),
        "uninstall should leave installed.json with an empty plugins array"
    );
    assert!(
        !KimiIntegration.has_tracedecay(home),
        "has_tracedecay should be false after uninstall"
    );
}

#[test]
fn test_kimi_has_tracedecay_via_plugin_entry_alone() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let kimi_code_home = home.join("kimi-code-home");
    let _kimi_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        &kimi_code_home,
    );
    let ctx = make_install_ctx(home);
    KimiIntegration.install(&ctx).unwrap();

    // The install never writes the legacy ~/.kimi tree: the installed.json
    // plugin entry alone must count as an installation.
    assert!(!home.join(".kimi").exists());
    assert!(KimiIntegration.has_tracedecay(home));
}

#[test]
fn test_opencode_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    // OpenCode uses ~/.config/opencode/opencode.json
    // Create the parent dir so install can discover it
    let ctx = make_install_ctx(home);
    OpenCodeIntegration.install(&ctx).unwrap();

    let config_path = home.join(".config/opencode/opencode.json");
    assert!(
        config_path.exists(),
        "opencode.json should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(content["mcp"]["tracedecay"].is_object());
}

#[test]
fn test_zed_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ZedIntegration.install(&ctx).unwrap();

    // On macOS: ~/Library/Application Support/Zed/settings.json
    // On linux: ~/.config/zed/settings.json
    #[cfg(target_os = "macos")]
    let settings_path = home.join("Library/Application Support/Zed/settings.json");
    #[cfg(not(target_os = "macos"))]
    let settings_path = home.join(".config/zed/settings.json");

    assert!(
        settings_path.exists(),
        "Zed settings.json should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(content["context_servers"]["tracedecay"].is_object());
}

#[test]
fn test_cline_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    ClineIntegration.install(&ctx).unwrap();

    // Cline registers in the current CLI/IDE data dir; the VS Code extension
    // global-storage path is retained only for legacy migration/removal.
    let settings_path = home.join(".cline/data/settings/cline_mcp_settings.json");

    assert!(
        settings_path.exists(),
        "Cline settings should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(content["mcpServers"]["tracedecay"].is_object());
}

#[test]
fn test_roo_code_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    RooCodeIntegration.install(&ctx).unwrap();

    #[cfg(target_os = "macos")]
    let settings_path = home.join("Library/Application Support/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
    #[cfg(target_os = "linux")]
    let settings_path = home.join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
    #[cfg(target_os = "windows")]
    let settings_path = home.join("AppData/Roaming/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let settings_path = home.join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");

    assert!(
        settings_path.exists(),
        "Roo Code settings should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(content["mcpServers"]["tracedecay"].is_object());
}

#[test]
fn test_copilot_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    CopilotIntegration.install(&ctx).unwrap();

    // Check VS Code settings.json
    #[cfg(target_os = "macos")]
    let vscode_settings = home.join("Library/Application Support/Code/User/settings.json");
    #[cfg(target_os = "linux")]
    let vscode_settings = home.join(".config/Code/User/settings.json");
    #[cfg(target_os = "windows")]
    let vscode_settings = home.join("AppData/Roaming/Code/User/settings.json");
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let vscode_settings = home.join(".config/Code/User/settings.json");

    assert!(
        vscode_settings.exists(),
        "VS Code settings.json should exist"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&vscode_settings).unwrap()).unwrap();
    assert!(content["mcp"]["servers"]["tracedecay"].is_object());

    // Check CLI config
    let cli_config = home.join(".copilot/mcp-config.json");
    assert!(cli_config.exists(), "Copilot CLI config should exist");
    let cli_content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cli_config).unwrap()).unwrap();
    assert!(cli_content["mcpServers"]["tracedecay"].is_object());

    let cli_prompt = home.join(".copilot/copilot-instructions.md");
    let prompt = std::fs::read_to_string(&cli_prompt).unwrap();
    assert!(prompt.contains("tracedecay_fact_store"));
    assert!(prompt.contains("tracedecay_active_project"));
    assert!(prompt.contains("tracedecay_storage_status"));
    assert!(prompt.contains("sensitive or proprietary code"));
}

#[test]
fn test_vibe_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    VibeIntegration.install(&ctx).unwrap();

    let config_path = home.join(".vibe/config.toml");
    assert!(
        config_path.exists(),
        "config.toml should exist after install"
    );
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("name = \"tracedecay\""),
        "config should contain tracedecay MCP server"
    );
    assert!(
        content.contains("transport = \"stdio\""),
        "config should use stdio transport"
    );
    assert!(
        content.contains("args = [\"serve\"]"),
        "config should have serve arg"
    );

    // Check prompt rules
    let prompt_path = home.join(".vibe/prompts/cli.md");
    assert!(
        prompt_path.exists(),
        "Vibe prompt should exist after install"
    );
    let prompt = std::fs::read_to_string(&prompt_path).unwrap();
    assert!(prompt.contains("tracedecay"));
    assert!(prompt.contains("tracedecay_fact_store"));
    assert!(prompt.contains("tracedecay_active_project"));
    assert!(prompt.contains("tracedecay_storage_status"));
    assert!(prompt.contains("sensitive or proprietary code"));
}

// ---------------------------------------------------------------------------
// 4. Install followed by Uninstall
// ---------------------------------------------------------------------------

#[test]
fn test_claude_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    let marketplace_manifest =
        home.join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/marketplace.json");

    // Install deploys the plugin bundle + registers the marketplace.
    ClaudeIntegration.install(&ctx).unwrap();
    assert!(
        marketplace_manifest.exists(),
        "plugin marketplace manifest should exist after install"
    );

    // Uninstall removes the deployed bundle, unregisters the marketplace, and
    // disables the plugin.
    ClaudeIntegration.uninstall(&ctx).unwrap();

    assert!(
        !marketplace_manifest.exists(),
        "deployed plugin bundle should be removed after uninstall"
    );
    let known_path = home.join(".claude/plugins/known_marketplaces.json");
    if known_path.exists() {
        let known: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&known_path).unwrap()).unwrap();
        assert!(
            known.get("tracedecay").is_none(),
            "tracedecay marketplace should be unregistered after uninstall"
        );
    }
    let settings_path = home.join(".claude/settings.json");
    if settings_path.exists() {
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert!(
            settings
                .get("enabledPlugins")
                .and_then(|v| v.get("tracedecay@tracedecay"))
                .is_none(),
            "plugin should be disabled after uninstall"
        );
    }
}

#[test]
fn test_claude_uninstall_unrecords_memory_digest_target() {
    let _env_lock = AGENT_ENV_LOCK.blocking_lock();
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let profile_root = home.join(".tracedecay");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
    let ctx = make_install_ctx(home);
    let claude_md = home.join(".claude/CLAUDE.md");

    ClaudeIntegration.install(&ctx).unwrap();
    assert!(
        std::fs::read_to_string(&claude_md)
            .unwrap()
            .contains(tracedecay::automation::memory_digest::MEMORY_DIGEST_START),
        "install should seed the prompt-index memory digest block"
    );

    ClaudeIntegration.uninstall(&ctx).unwrap();

    std::fs::create_dir_all(claude_md.parent().unwrap()).unwrap();
    std::fs::write(&claude_md, "# Claude rules\n").unwrap();
    tracedecay::automation::memory_digest::export_memory_digest_to_recorded_targets(&profile_root)
        .unwrap();
    assert!(
        !std::fs::read_to_string(&claude_md)
            .unwrap()
            .contains(tracedecay::automation::memory_digest::MEMORY_DIGEST_START),
        "Claude uninstall must unrecord memory digest targets so refresh cannot recreate them"
    );
}

#[test]
fn test_gemini_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    GeminiIntegration.install(&ctx).unwrap();
    let settings_path = home.join(".gemini/settings.json");
    assert!(settings_path.exists());

    GeminiIntegration.uninstall(&ctx).unwrap();

    // After uninstall, settings.json should be removed or not contain tracedecay
    if settings_path.exists() {
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        let has_tracedecay = content
            .get("mcpServers")
            .and_then(|v| v.get("tracedecay"))
            .is_some();
        assert!(
            !has_tracedecay,
            "tracedecay should be removed from settings.json"
        );
    }

    // GEMINI.md should be removed (was only tracedecay rules)
    let gemini_md = home.join(".gemini/GEMINI.md");
    if gemini_md.exists() {
        let content = std::fs::read_to_string(&gemini_md).unwrap();
        assert!(
            !content.contains("## Prefer tracedecay MCP tools"),
            "GEMINI.md should not contain tracedecay rules after uninstall"
        );
    }
}

#[test]
fn test_claude_install_preserves_existing_config() {
    // The Claude plugin install merges into settings.json and
    // known_marketplaces.json rather than owning a single user-editable config
    // file, so preservation is checked against those two files directly: a
    // foreign settings key and a foreign registered marketplace must survive.
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let claude_dir = home.join(".claude");
    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();

    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{ "theme": "solarized" }"#,
    )
    .unwrap();
    std::fs::write(
        plugins_dir.join("known_marketplaces.json"),
        r#"{ "other": { "source": { "source": "directory", "path": "/somewhere" } } }"#,
    )
    .unwrap();

    ClaudeIntegration
        .install(&make_install_ctx(home))
        .expect("install should succeed");

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        settings["theme"].as_str(),
        Some("solarized"),
        "existing settings.json key must be preserved"
    );
    assert_eq!(
        settings["enabledPlugins"]["tracedecay@tracedecay"],
        serde_json::json!(true),
        "install must still enable the plugin alongside the existing key"
    );

    let known: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(plugins_dir.join("known_marketplaces.json")).unwrap(),
    )
    .unwrap();
    assert!(
        known.get("other").is_some(),
        "existing foreign marketplace must be preserved"
    );
    assert_eq!(
        known["tracedecay"]["source"]["source"].as_str(),
        Some("directory"),
        "install must register the tracedecay marketplace alongside the foreign one"
    );
}

#[test]
fn test_gemini_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "theme": "dark",
  "mcpServers": { "other": { "command": "other-bin" } }
}
"#;
    assert_install_backs_up_and_preserves(&GeminiIntegration, dir.path(), original, "\"theme\"");
}

#[test]
fn test_opencode_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "$schema": "https://opencode.ai/config.json",
  "mcp": { "other": { "type": "local", "command": ["other-bin"] } }
}
"#;
    assert_install_backs_up_and_preserves(&OpenCodeIntegration, dir.path(), original, "other-bin");
}

#[test]
fn test_zed_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "theme": "One Dark",
  "context_servers": { "other": { "command": { "path": "other-bin", "args": [] } } }
}
"#;
    assert_install_backs_up_and_preserves(&ZedIntegration, dir.path(), original, "One Dark");
}

#[test]
fn test_cline_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "mcpServers": { "other": { "command": "other-bin" } }
}
"#;
    assert_install_backs_up_and_preserves(&ClineIntegration, dir.path(), original, "other-bin");
}

#[test]
fn test_roo_code_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "mcpServers": { "other": { "command": "other-bin" } }
}
"#;
    assert_install_backs_up_and_preserves(&RooCodeIntegration, dir.path(), original, "other-bin");
}

#[test]
fn test_kilo_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  // user comment about their workflow
  "mcp": { "other": { "type": "local", "command": ["other-bin"], "enabled": true } }
}
"#;
    assert_install_backs_up_and_preserves(&KiloIntegration, dir.path(), original, "other-bin");
}

#[test]
fn test_antigravity_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "mcpServers": { "other": { "command": "other-bin" } }
}
"#;
    assert_install_backs_up_and_preserves(
        &AntigravityIntegration,
        dir.path(),
        original,
        "other-bin",
    );
}

/// Regression for #85: `tracedecay install --agent antigravity` must populate
/// both the IDE config and the CLI plugin file so the `agy` CLI can see the
/// server. Before the fix only the IDE path was written, which left the CLI
/// invisible in `/mcp`.
#[test]
fn test_antigravity_install_writes_cli_plugin() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let bin = "/usr/local/bin/tracedecay";
    let ctx = InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: bin.to_string(),
        tool_permissions: expected_tool_perms(),
        project_root: None,
        dashboard: false,
    };

    AntigravityIntegration.install(&ctx).expect("install ok");

    let ide_path = home.join(".gemini/antigravity/mcp_config.json");
    let cli_path = home.join(".gemini/antigravity-cli/plugins/tracedecay.json");
    assert!(
        ide_path.exists(),
        "IDE config must be written: {ide_path:?}"
    );
    assert!(
        cli_path.exists(),
        "CLI plugin must be written: {cli_path:?}"
    );

    for path in [&ide_path, &cli_path] {
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let server = body
            .get("mcpServers")
            .and_then(|v| v.get("tracedecay"))
            .expect("tracedecay entry");
        assert_eq!(
            server.get("command").and_then(|v| v.as_str()),
            Some(bin),
            "{path:?} must point at the install bin"
        );
        assert!(
            server
                .get("args")
                .and_then(|v| v.as_array())
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("serve"))),
            "{path:?} must invoke `serve`"
        );
    }
}

/// Uninstall must remove the CLI plugin file outright (it belongs only to
/// tracedecay) and remove the `tracedecay` entry from the shared IDE config
/// without touching the user's other entries.
#[test]
fn test_antigravity_uninstall_removes_both_locations() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let bin = "/usr/local/bin/tracedecay";
    let ctx = InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: bin.to_string(),
        tool_permissions: expected_tool_perms(),
        project_root: None,
        dashboard: false,
    };

    AntigravityIntegration.install(&ctx).unwrap();
    AntigravityIntegration.uninstall(&ctx).unwrap();

    let cli_path = home.join(".gemini/antigravity-cli/plugins/tracedecay.json");
    assert!(
        !cli_path.exists(),
        "CLI plugin file must be deleted, still exists at {cli_path:?}"
    );

    let ide_path = home.join(".gemini/antigravity/mcp_config.json");
    // IDE config either deleted (empty) or rewritten without our entry —
    // both are acceptable; what's not acceptable is the entry persisting.
    if ide_path.exists() {
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&ide_path).unwrap()).unwrap();
        assert!(
            body.get("mcpServers")
                .and_then(|v| v.get("tracedecay"))
                .is_none(),
            "tracedecay entry must be removed from {ide_path:?}"
        );
    }
}

#[test]
fn test_copilot_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "editor.fontSize": 14,
  "workbench.colorTheme": "Default Dark+"
}
"#;
    assert_install_backs_up_and_preserves(
        &CopilotIntegration,
        dir.path(),
        original,
        "Default Dark+",
    );
}

/// Meta-test: every agent that goes through `assert_install_backs_up_and_preserves`
/// above must also actually return a path from `primary_config_path`. Catches
/// the case where a new integration is added without wiring up the method,
/// which would otherwise only surface as a confusing panic in CI.
#[test]
fn test_every_tested_agent_advertises_primary_config_path() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    // Kimi's primary config resolves through KIMI_CODE_HOME; pin it under the
    // temp home so a developer-set override cannot point the assertion outside
    // `home` (and hold the env lock while the guard is alive).
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let _kimi_code_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        home.join(".kimi-code"),
    );
    let agents: Vec<(&dyn AgentIntegration, &str)> = vec![
        (&ClaudeIntegration, "claude"),
        (&GeminiIntegration, "gemini"),
        (&CursorIntegration, "cursor"),
        (&OpenCodeIntegration, "opencode"),
        (&ZedIntegration, "zed"),
        (&ClineIntegration, "cline"),
        (&RooCodeIntegration, "roo-code"),
        (&CopilotIntegration, "copilot"),
        (&KiloIntegration, "kilo"),
        (&AntigravityIntegration, "antigravity"),
        (&CodexIntegration, "codex"),
        (&KiroIntegration, "kiro"),
        (&KimiIntegration, "kimi"),
    ];
    for (agent, id) in agents {
        let path = agent
            .primary_config_path(home)
            .unwrap_or_else(|| panic!("{id} must implement primary_config_path"));
        assert!(
            path.starts_with(home),
            "{id}: primary_config_path must be under the home arg, got {}",
            path.display()
        );
    }
}

#[test]
fn test_copilot_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    CopilotIntegration.install(&ctx).unwrap();
    CopilotIntegration.uninstall(&ctx).unwrap();

    // CLI config should be cleaned up
    let cli_config = home.join(".copilot/mcp-config.json");
    if cli_config.exists() {
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cli_config).unwrap()).unwrap();
        let has_tracedecay = content
            .get("mcpServers")
            .and_then(|v| v.get("tracedecay"))
            .is_some();
        assert!(!has_tracedecay);
    }
}

#[test]
fn test_vibe_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    VibeIntegration.install(&ctx).unwrap();
    VibeIntegration.uninstall(&ctx).unwrap();

    let config_path = home.join(".vibe/config.toml");
    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            !content.contains("name = \"tracedecay\""),
            "tracedecay should be removed from config.toml"
        );
    }

    let prompt_path = home.join(".vibe/prompts/cli.md");
    if prompt_path.exists() {
        let content = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(
            !content.contains("tracedecay"),
            "tracedecay rules should be removed from prompt"
        );
    }
}

// ---------------------------------------------------------------------------
// 8. Idempotency tests
// ---------------------------------------------------------------------------

#[test]
fn test_claude_install_idempotent() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    // Install twice should not fail
    ClaudeIntegration.install(&ctx).unwrap();
    ClaudeIntegration.install(&ctx).unwrap();

    // The plugin should remain installed and enabled (idempotent).
    assert!(
        home.join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/marketplace.json")
            .exists(),
        "marketplace manifest should still be deployed after a second install"
    );
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        settings["enabledPlugins"]["tracedecay@tracedecay"],
        serde_json::json!(true),
        "plugin should stay enabled after a second install"
    );
}

#[test]
fn test_gemini_install_idempotent() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    GeminiIntegration.install(&ctx).unwrap();
    GeminiIntegration.install(&ctx).unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".gemini/settings.json")).unwrap())
            .unwrap();
    assert!(settings["mcpServers"]["tracedecay"].is_object());
}

#[test]
fn test_uninstall_without_install_does_not_crash() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    // Uninstalling when nothing is installed should not panic or error
    ClaudeIntegration.uninstall(&ctx).unwrap();
    GeminiIntegration.uninstall(&ctx).unwrap();
    CodexIntegration.uninstall(&ctx).unwrap();
    CursorIntegration.uninstall(&ctx).unwrap();
    CopilotIntegration.uninstall(&ctx).unwrap();
    OpenCodeIntegration.uninstall(&ctx).unwrap();
    ZedIntegration.uninstall(&ctx).unwrap();
    ClineIntegration.uninstall(&ctx).unwrap();
    RooCodeIntegration.uninstall(&ctx).unwrap();
    KiroIntegration.uninstall(&ctx).unwrap();
    VibeIntegration.uninstall(&ctx).unwrap();
}

// ---------------------------------------------------------------------------
// 9. Install preserves existing config
// ---------------------------------------------------------------------------

#[test]
fn test_claude_install_preserves_existing_claude_json() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);

    // Pre-populate .claude.json with a foreign MCP server and a custom key.
    // The plugin model no longer writes tracedecay into ~/.claude.json, and the
    // install's config-managed migration must leave unrelated entries intact.
    let claude_json_path = home.join(".claude.json");
    std::fs::write(
        &claude_json_path,
        r#"{"mcpServers": {"other-server": {"command": "foo"}}, "customKey": 42}"#,
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json_path).unwrap()).unwrap();
    // tracedecay must NOT be added to ~/.claude.json (plugin provides the server)
    assert!(
        content["mcpServers"].get("tracedecay").is_none(),
        "install must not write tracedecay into ~/.claude.json"
    );
    // existing foreign server preserved
    assert!(content["mcpServers"]["other-server"].is_object());
    // custom key preserved
    assert_eq!(content["customKey"], 42);
}

#[test]
fn test_gemini_install_preserves_existing_settings() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);

    let settings_path = home.join(".gemini/settings.json");
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    std::fs::write(
        &settings_path,
        r#"{"mcpServers": {"other": {"command": "bar"}}, "theme": "dark"}"#,
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    GeminiIntegration.install(&ctx).unwrap();

    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(content["mcpServers"]["tracedecay"].is_object());
    assert!(content["mcpServers"]["other"].is_object());
    assert_eq!(content["theme"], "dark");
}

// ---------------------------------------------------------------------------

//! `tracedecay update-plugin` contract tests.
//!
//! The command refreshes tracedecay-generated artifacts (plugin code, baked
//! binary paths, embedded assets) for detected installs and must leave every
//! agent config file byte-for-byte intact — pins, user keys, MCP entries,
//! settings. These tests hash configs before/after `update_plugin` per agent
//! to prove that contract, and assert the artifacts actually got re-baked.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::agents::{get_integration, InstallContext, UpdatePluginOutcome};

const OLD_BIN: &str = "/old/bin/tracedecay";
const NEW_BIN: &str = "/new/bin/tracedecay";

fn ctx(home: &Path, tracedecay_bin: &str) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: tracedecay_bin.to_string(),
        tool_permissions: tracedecay::agents::expected_tool_perms(),
        profile: None,
        project_root: None,
        dashboard: true,
    }
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Every regular file under `root`, relative to it, sorted.
fn file_listing(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Hermes
// ---------------------------------------------------------------------------

#[test]
fn hermes_update_plugin_refreshes_all_profiles_without_touching_config() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let hermes = get_integration("hermes").unwrap();

    // Default-profile install with a pinned project root + dashboard page.
    let mut install_ctx = ctx(home.path(), OLD_BIN);
    install_ctx.project_root = Some(project.path().to_path_buf());
    hermes.install(&install_ctx).unwrap();

    // Named-profile install without a dashboard (`--no-dashboard`).
    let mut profile_ctx = ctx(home.path(), OLD_BIN);
    profile_ctx.profile = Some("work".to_string());
    profile_ctx.dashboard = false;
    hermes.install(&profile_ctx).unwrap();

    // Simulate user customization a YAML rewrite could disturb.
    let default_config = home.path().join(".hermes/config.yaml");
    let work_config = home.path().join(".hermes/profiles/work/config.yaml");
    let mut customized = text(&default_config);
    customized.push_str("\n# user comment\nui:\n  theme: dark\n");
    std::fs::write(&default_config, &customized).unwrap();

    let default_config_before = bytes(&default_config);
    let work_config_before = bytes(&work_config);

    let outcome = hermes.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    let UpdatePluginOutcome::Refreshed(paths) = outcome else {
        panic!("expected hermes update_plugin to refresh detected installs");
    };
    let default_plugin = home.path().join(".hermes/plugins/tracedecay");
    let work_plugin = home.path().join(".hermes/profiles/work/plugins/tracedecay");
    assert!(paths.contains(&default_plugin), "missing default profile");
    assert!(paths.contains(&work_plugin), "missing named profile");

    // Configs byte-identical: pin, user keys, and comments intact.
    assert_eq!(bytes(&default_config), default_config_before);
    assert_eq!(bytes(&work_config), work_config_before);

    // Artifacts re-baked with the new binary path and current version stamp.
    for plugin_dir in [&default_plugin, &work_plugin] {
        assert!(text(&plugin_dir.join("tools.py")).contains(NEW_BIN));
        assert!(text(&plugin_dir.join("plugin.yaml"))
            .contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))));
    }

    // Dashboard page refreshed where deployed, with the pin re-read from
    // config.yaml and re-baked into plugin_api.py.
    let api = text(&default_plugin.join("dashboard/plugin_api.py"));
    assert!(api.contains(NEW_BIN));
    // The pin is baked in as a JSON-encoded Python string literal, so match
    // the encoded form (Windows backslashes are escaped in the artifact).
    let pinned_json = serde_json::to_string(&project.path().display().to_string()).unwrap();
    assert!(
        api.contains(&pinned_json),
        "plugin_api.py should bake the project-root pin, missing {pinned_json}"
    );
    assert!(
        text(&default_plugin.join("dashboard/manifest.json")).contains(env!("CARGO_PKG_VERSION"))
    );

    // A `--no-dashboard` install stays dashboard-free.
    assert!(!work_plugin.join("dashboard").exists());
}

#[test]
fn hermes_update_plugin_succeeds_where_a_config_rewrite_would_refuse() {
    let home = TempDir::new().unwrap();
    let hermes = get_integration("hermes").unwrap();
    hermes.install(&ctx(home.path(), OLD_BIN)).unwrap();

    // Flow-style `plugins:` mapping — the refuse-don't-rewrite YAML guard
    // makes install/reinstall error on this shape.
    let config = home.path().join(".hermes/config.yaml");
    std::fs::write(&config, "plugins: {enabled: [tracedecay]}\n").unwrap();
    let config_before = bytes(&config);
    assert!(
        hermes.install(&ctx(home.path(), NEW_BIN)).is_err(),
        "sanity: reinstall-style install must refuse this config shape"
    );

    // update-plugin never parses-to-write config.yaml, so it succeeds and
    // still refreshes the generated artifacts.
    let outcome = hermes.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::Refreshed(_)));
    assert_eq!(bytes(&config), config_before);
    assert!(text(&home.path().join(".hermes/plugins/tracedecay/tools.py")).contains(NEW_BIN));
}

#[test]
fn hermes_update_plugin_reports_not_installed_when_nothing_is_detected() {
    let home = TempDir::new().unwrap();
    // A Hermes home without a generated plugin must not be installed into.
    std::fs::create_dir_all(home.path().join(".hermes")).unwrap();
    let hermes = get_integration("hermes").unwrap();
    let outcome = hermes.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::NotInstalled));
    assert!(!home.path().join(".hermes/plugins").exists());
    assert!(!home.path().join(".hermes/config.yaml").exists());
}

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

#[test]
fn cursor_update_plugin_refreshes_bundle_and_preserves_user_config() {
    let home = TempDir::new().unwrap();
    let cursor = get_integration("cursor").unwrap();

    // User-owned Cursor config that update-plugin must never write.
    let user_mcp = home.path().join(".cursor/mcp.json");
    std::fs::create_dir_all(user_mcp.parent().unwrap()).unwrap();
    std::fs::write(
        &user_mcp,
        "{\n  \"mcpServers\": {\n    \"other\": { \"command\": \"other-bin\" }\n  }\n}\n",
    )
    .unwrap();

    cursor.install(&ctx(home.path(), OLD_BIN)).unwrap();
    let plugin_dir = home.path().join(".cursor/plugins/local/tracedecay");

    // An unmanaged user file inside the plugin dir must survive the refresh.
    std::fs::write(plugin_dir.join("user-note.txt"), "mine\n").unwrap();
    let user_mcp_before = bytes(&user_mcp);

    let outcome = cursor.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    let UpdatePluginOutcome::Refreshed(paths) = outcome else {
        panic!("expected cursor update_plugin to refresh the bundle");
    };
    assert_eq!(paths, vec![plugin_dir.clone()]);

    // User config byte-identical; unmanaged file preserved.
    assert_eq!(bytes(&user_mcp), user_mcp_before);
    assert_eq!(text(&plugin_dir.join("user-note.txt")), "mine\n");

    // Generated bundle re-baked: plugin-owned mcp.json command, hook command
    // paths, and the manifest version stamp.
    assert!(text(&plugin_dir.join("mcp.json")).contains(NEW_BIN));
    assert!(text(&plugin_dir.join("hooks/hooks.json")).contains(NEW_BIN));
    assert!(
        text(&plugin_dir.join(".cursor-plugin/plugin.json")).contains(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn cursor_update_plugin_reports_not_installed_without_a_bundle() {
    let home = TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join(".cursor")).unwrap();
    let cursor = get_integration("cursor").unwrap();
    let outcome = cursor.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::NotInstalled));
    assert!(!home.path().join(".cursor/plugins").exists());
}

// ---------------------------------------------------------------------------
// Kiro
// ---------------------------------------------------------------------------

#[test]
fn kiro_update_plugin_rebakes_managed_agent_and_preserves_configs() {
    let home = TempDir::new().unwrap();
    let kiro = get_integration("kiro").unwrap();
    kiro.install(&ctx(home.path(), OLD_BIN)).unwrap();

    let kiro_home = home.path().join(".kiro");
    let mcp_config = kiro_home.join("settings/mcp.json");
    let cli_config = kiro_home.join("settings/cli.json");
    let steering = kiro_home.join("steering/tracedecay.md");
    let agent_file = kiro_home.join("agents/tracedecay.json");

    let mcp_before = bytes(&mcp_config);
    let steering_before = bytes(&steering);
    let cli_before = cli_config.exists().then(|| bytes(&cli_config));

    let outcome = kiro.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    let UpdatePluginOutcome::Refreshed(paths) = outcome else {
        panic!("expected kiro update_plugin to refresh the managed agent");
    };
    assert_eq!(paths, vec![agent_file.clone()]);

    // Shared configs and steering byte-identical.
    assert_eq!(bytes(&mcp_config), mcp_before);
    assert_eq!(bytes(&steering), steering_before);
    if let Some(cli_before) = cli_before {
        assert_eq!(bytes(&cli_config), cli_before);
    }

    // Managed agent hooks re-baked with the new binary path.
    let agent = text(&agent_file);
    assert!(agent.contains(NEW_BIN));
    assert!(!agent.contains(OLD_BIN));
}

#[test]
fn kiro_update_plugin_leaves_user_managed_agent_files_alone() {
    let home = TempDir::new().unwrap();
    let kiro = get_integration("kiro").unwrap();

    let agent_file = home.path().join(".kiro/agents/tracedecay.json");
    std::fs::create_dir_all(agent_file.parent().unwrap()).unwrap();
    std::fs::write(
        &agent_file,
        "{\n  \"name\": \"tracedecay\",\n  \"description\": \"my own agent\"\n}\n",
    )
    .unwrap();
    let before = bytes(&agent_file);

    let outcome = kiro.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::NotInstalled));
    assert_eq!(bytes(&agent_file), before);
}

// ---------------------------------------------------------------------------
// Config-only integrations
// ---------------------------------------------------------------------------

#[test]
fn config_only_integrations_report_config_only_and_write_nothing() {
    // These agents keep their entire tracedecay integration inside shared
    // config files (MCP entries, hook blocks, prompt rules); update-plugin
    // must not create or modify a single file for them.
    let config_only = [
        "claude",
        "opencode",
        "codex",
        "gemini",
        "copilot",
        "zed",
        "cline",
        "roo-code",
        "antigravity",
        "kilo",
        "kimi",
        "vibe",
    ];
    for id in config_only {
        let home = TempDir::new().unwrap();
        let agent = get_integration(id).unwrap();
        let outcome = agent.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
        assert!(
            matches!(outcome, UpdatePluginOutcome::ConfigOnly),
            "{id} should be config-only"
        );
        assert!(
            file_listing(home.path()).is_empty(),
            "{id} update_plugin wrote files into the home dir"
        );
    }
}

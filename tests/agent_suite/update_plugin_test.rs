//! `tracedecay update-plugin` contract tests.
//!
//! The command refreshes tracedecay-generated artifacts (plugin code, baked
//! binary paths, embedded assets) for detected installs and must leave every
//! agent config file byte-for-byte intact — pins, user keys, MCP entries,
//! settings. These tests hash configs before/after `update_plugin` per agent
//! to prove that contract, and assert the artifacts actually got re-baked.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;
use tracedecay::agents::{InstallContext, UpdatePluginOutcome, get_integration};

use crate::common::{AgentEnvLock, EnvVarGuard};
use crate::plugin_validation_support::{assert_schema_valid, compile_schema, relative_files_under};

const OLD_BIN: &str = "/old/bin/tracedecay";
const NEW_BIN: &str = "/new/bin/tracedecay";

fn ctx(home: &Path, tracedecay_bin: &str) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: tracedecay_bin.to_string(),
        tool_permissions: tracedecay::agents::expected_tool_perms(),
        project_root: None,
        dashboard: true,
    }
}

fn ctx_with_project(home: &Path, tracedecay_bin: &str, project_root: &Path) -> InstallContext {
    let mut ctx = ctx(home, tracedecay_bin);
    ctx.project_root = Some(project_root.to_path_buf());
    ctx
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&text(path))
        .unwrap_or_else(|e| panic!("failed to parse JSON {}: {e}", path.display()))
}

/// The scope contract a rendered Codex bundle must follow: global bundles
/// must ship lifecycle hooks, repo-local bundles must not. Kept explicit so a
/// refresh path that silently stops rendering hooks/hooks.json fails the
/// global assertions instead of skipping them.
#[derive(Clone, Copy, PartialEq)]
enum CodexScope {
    Global,
    RepoLocal,
}

fn assert_codex_bundle_contains_bin(plugin_dir: &Path, tracedecay_bin: &str, scope: CodexScope) {
    let tracedecay_bin = tracedecay_bin.replace('\\', "/");
    assert!(text(&plugin_dir.join(".mcp.json")).contains(&tracedecay_bin));
    let hooks_path = plugin_dir.join("hooks/hooks.json");
    match scope {
        CodexScope::Global => {
            assert!(
                hooks_path.exists(),
                "global Codex bundle {} must ship hooks/hooks.json",
                plugin_dir.display()
            );
            assert!(text(&hooks_path).contains(&tracedecay_bin));
        }
        CodexScope::RepoLocal => assert!(
            !hooks_path.exists(),
            "repo-local Codex bundle {} must not ship lifecycle hooks",
            plugin_dir.display()
        ),
    }
}

fn codex_bootstrap_dir(home: &Path) -> PathBuf {
    home.join("plugins/tracedecay")
}

fn codex_cached_plugin_dir(home: &Path) -> PathBuf {
    home.join(".codex/plugins/cache/personal/tracedecay")
        .join(env!("CARGO_PKG_VERSION"))
}

// ---------------------------------------------------------------------------
// Hermes
// ---------------------------------------------------------------------------

#[test]
fn hermes_update_plugin_refreshes_default_and_named_profile_installs() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(home.path());
    let _hermes_home = EnvVarGuard::unset("HERMES_HOME");
    let hermes = get_integration("hermes").unwrap();

    hermes.install(&ctx(home.path(), OLD_BIN)).unwrap();

    // Named Hermes profiles are independent host homes and are refreshed too.
    let work_plugin = home.path().join(".hermes/profiles/work/plugins/tracedecay");
    std::fs::create_dir_all(&work_plugin).unwrap();
    std::fs::write(work_plugin.join("plugin.yaml"), "name: tracedecay\n").unwrap();
    std::fs::write(work_plugin.join("tools.py"), OLD_BIN).unwrap();
    let work_config = home.path().join(".hermes/profiles/work/config.yaml");
    std::fs::write(&work_config, "plugins:\n  enabled:\n    - tracedecay\n").unwrap();

    // Simulate user customization a YAML rewrite could disturb.
    let default_config = home.path().join(".hermes/config.yaml");
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
    assert_eq!(paths, vec![default_plugin.clone(), work_plugin.clone()]);

    // The supported user config remains byte-identical.
    assert_eq!(bytes(&default_config), default_config_before);
    assert_eq!(bytes(&work_config), work_config_before);

    // Artifacts re-baked with the new binary path and current version stamp.
    assert!(text(&default_plugin.join("tools.py")).contains(NEW_BIN));
    assert!(
        text(&default_plugin.join("plugin.yaml"))
            .contains(&format!("version: {}", env!("CARGO_PKG_VERSION")))
    );
    assert!(text(&work_plugin.join("tools.py")).contains(NEW_BIN));
    assert!(work_plugin.join("plugin.yaml").exists());
    assert!(text(&work_config).contains("tracedecay"));

    // Dashboard page refreshes without a Hermes profile/project default.
    let api = text(&default_plugin.join("dashboard/plugin_api.py"));
    assert!(api.contains(NEW_BIN));
    assert!(!api.contains("DEPLOYED_PROJECT_ROOT"));
    assert!(
        text(&default_plugin.join("dashboard/manifest.json")).contains(env!("CARGO_PKG_VERSION"))
    );

    assert!(!work_plugin.join("dashboard").exists());
}

#[test]
fn hermes_update_plugin_succeeds_where_a_config_rewrite_would_refuse() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(home.path());
    let _hermes_home = EnvVarGuard::unset("HERMES_HOME");
    let hermes = get_integration("hermes").unwrap();
    hermes.install(&ctx(home.path(), OLD_BIN)).unwrap();

    // A top-level non-mapping config — the lossless editor supports every
    // parseable mapping shape (including flow styles), so only a config that
    // cannot be a profile mapping still makes install/reinstall refuse.
    let config = home.path().join(".hermes/config.yaml");
    std::fs::write(&config, "- not a profile mapping\n").unwrap();
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
    let _agent_env = AgentEnvLock::pin(home.path());
    let _hermes_home = EnvVarGuard::unset("HERMES_HOME");
    // A Hermes home without a generated plugin must not be installed into.
    std::fs::create_dir_all(home.path().join(".hermes")).unwrap();
    let hermes = get_integration("hermes").unwrap();
    let outcome = hermes.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::NotInstalled));
    assert!(!home.path().join(".hermes/plugins").exists());
    assert!(!home.path().join(".hermes/config.yaml").exists());
}

#[test]
fn hermes_update_plugin_refreshes_named_only_install_in_place() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(home.path());
    let _hermes_home = EnvVarGuard::unset("HERMES_HOME");
    let legacy = home.path().join(".hermes/profiles/work/plugins/tracedecay");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("plugin.yaml"), "name: tracedecay\n").unwrap();
    std::fs::write(legacy.join("tools.py"), OLD_BIN).unwrap();
    std::fs::write(
        home.path().join(".hermes/profiles/work/config.yaml"),
        "plugins:\n  enabled:\n    - tracedecay\n",
    )
    .unwrap();

    let outcome = get_integration("hermes")
        .unwrap()
        .update_plugin(&ctx(home.path(), NEW_BIN))
        .unwrap();

    assert!(matches!(
        outcome,
        UpdatePluginOutcome::Refreshed(paths) if paths == vec![legacy.clone()]
    ));
    assert!(text(&legacy.join("tools.py")).contains(NEW_BIN));
    assert!(legacy.join("plugin.yaml").exists());
    assert!(!home.path().join(".hermes/plugins/tracedecay").exists());
}

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

#[test]
fn cursor_update_plugin_refreshes_bundle_and_preserves_user_config() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
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
    // A retired TraceDecay-authored skill must be removed while a same-name
    // user-authored workflow stays untouched.
    std::fs::create_dir_all(plugin_dir.join("skills/reading-code-cheaply")).unwrap();
    std::fs::write(
        plugin_dir.join("skills/reading-code-cheaply/SKILL.md"),
        "---\nname: reading-code-cheaply\ndescription: retired tracedecay skill\n---\n\nUse TraceDecay MCP tools for this workflow.\n",
    )
    .unwrap();
    std::fs::create_dir_all(plugin_dir.join("skills/project-status")).unwrap();
    std::fs::write(
        plugin_dir.join("skills/project-status/SKILL.md"),
        "---\nname: project-status\ndescription: My private project status workflow\n---\n",
    )
    .unwrap();
    let user_mcp_before = bytes(&user_mcp);

    let outcome = cursor.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    let UpdatePluginOutcome::Refreshed(paths) = outcome else {
        panic!("expected cursor update_plugin to refresh the bundle");
    };
    assert_eq!(paths, vec![plugin_dir.clone()]);

    // User config byte-identical; unmanaged file preserved.
    assert_eq!(bytes(&user_mcp), user_mcp_before);
    assert_eq!(text(&plugin_dir.join("user-note.txt")), "mine\n");
    assert!(
        !plugin_dir
            .join("skills/reading-code-cheaply/SKILL.md")
            .exists(),
        "update-plugin must remove retired TraceDecay-authored Cursor skills"
    );
    assert!(
        plugin_dir.join("skills/project-status/SKILL.md").exists(),
        "same-name user-authored Cursor skills without TraceDecay markers must be preserved"
    );

    // Generated bundle re-baked: plugin-owned mcp.json command, hook command
    // paths, and the manifest version stamp.
    assert!(text(&plugin_dir.join("mcp.json")).contains(NEW_BIN));
    // The `--path ${workspaceFolder}` args pin is asserted by
    // `assert_cursor_rendered_bundle_valid`, which
    // `cursor_update_plugin_rerenders_structurally_valid_bundle` runs against
    // this same update flow.
    assert!(text(&plugin_dir.join("hooks/hooks.json")).contains(NEW_BIN));
    assert!(
        text(&plugin_dir.join(".cursor-plugin/plugin.json")).contains(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn cursor_update_plugin_reports_not_installed_without_a_bundle() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    std::fs::create_dir_all(home.path().join(".cursor")).unwrap();
    let cursor = get_integration("cursor").unwrap();
    let outcome = cursor.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::NotInstalled));
    assert!(!home.path().join(".cursor/plugins").exists());
}

#[test]
fn claude_update_plugin_refreshes_bundle_and_preserves_user_config() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    let claude = get_integration("claude").unwrap();

    // User-owned Claude config that update-plugin must never destroy.
    let user_claude_json = home.path().join(".claude.json");
    std::fs::write(
        &user_claude_json,
        "{\n  \"mcpServers\": { \"other\": { \"command\": \"other-bin\" } }\n}\n",
    )
    .unwrap();

    claude.install(&ctx(home.path(), OLD_BIN)).unwrap();
    let deploy_dir = home.path().join(".claude/plugins/marketplaces/tracedecay");
    let user_json_before = bytes(&user_claude_json);

    let outcome = claude.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    let UpdatePluginOutcome::DeferredUserAction(deferred) = outcome else {
        panic!("Claude update_plugin must defer its host-native cache refresh");
    };
    assert_eq!(deferred.staged_paths, vec![deploy_dir.clone()]);
    assert!(
        deferred
            .remediation
            .contains("claude plugin update tracedecay@tracedecay")
    );

    // Foreign user config is byte-identical after the refresh.
    assert_eq!(bytes(&user_claude_json), user_json_before);

    // The deployed bundle is re-rendered with the new bin path and version.
    assert!(text(&deploy_dir.join(".mcp.json")).contains(NEW_BIN));
    assert!(text(&deploy_dir.join("hooks/hooks.json")).contains(NEW_BIN));
    assert!(
        text(&deploy_dir.join(".claude-plugin/plugin.json")).contains(env!("CARGO_PKG_VERSION"))
    );
}

/// `update-plugin` stages the source bundle without claiming it refreshed
/// Claude's native cache or rewriting settings and steering owned by the host.
#[test]
fn claude_update_plugin_defers_without_rewriting_settings_or_steering() {
    let home = TempDir::new().unwrap();
    let claude = get_integration("claude").unwrap();

    claude.install(&ctx(home.path(), OLD_BIN)).unwrap();

    let settings_path = home.path().join(".claude/settings.json");
    let claude_md_path = home.path().join(".claude/CLAUDE.md");

    // Simulate an older install: strip the plugin-namespace entries, add an
    // unrelated permission, and overwrite CLAUDE.md with a stale managed block.
    let mut settings = read_json(&settings_path);
    let allow = settings["permissions"]["allow"].as_array_mut().unwrap();
    allow.retain(|v| {
        !v.as_str()
            .is_some_and(|s| s.starts_with("mcp__plugin_tracedecay_graph__"))
    });
    allow.push(serde_json::json!("Bash(*)"));
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &claude_md_path,
        "# Project\n\n## MANDATORY: No Explore Agents When Tracedecay Is Available\n\nstale body\n",
    )
    .unwrap();
    let settings_before = bytes(&settings_path);
    let claude_md_before = bytes(&claude_md_path);

    let outcome = claude.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(
        outcome,
        UpdatePluginOutcome::DeferredUserAction(_)
    ));
    assert_eq!(bytes(&settings_path), settings_before);
    assert_eq!(bytes(&claude_md_path), claude_md_before);
}

#[test]
fn claude_update_plugin_reports_not_installed_without_a_bundle() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    let claude = get_integration("claude").unwrap();
    let outcome = claude.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::NotInstalled));
    assert!(
        !home
            .path()
            .join(".claude/plugins/marketplaces/tracedecay")
            .exists()
    );
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

#[test]
fn codex_update_plugin_reports_not_installed_without_source_or_native_cache() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    let project_root = home.path().join("workspace");
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    let codex = get_integration("codex").unwrap();
    let outcome = codex
        .update_plugin(&ctx_with_project(home.path(), NEW_BIN, &project_root))
        .unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::NotInstalled));
    assert!(!home.path().join("plugins").exists());
}

#[test]
fn codex_update_plugin_ignores_plugin_only_config_without_legacy_mcp() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    let project_root = home.path().join("workspace");
    let codex_dir = home.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let config = codex_dir.join("config.toml");
    std::fs::write(
        &config,
        r#"
[plugins."tracedecay@personal"]
enabled = true

[hooks.state."tracedecay@personal:hooks/hooks.json:post_tool_use:0:0"]
trusted_hash = "sha256:post"
"#,
    )
    .unwrap();
    let before = bytes(&config);
    let codex = get_integration("codex").unwrap();

    let outcome = codex
        .update_plugin(&ctx_with_project(home.path(), NEW_BIN, &project_root))
        .unwrap();

    assert!(matches!(outcome, UpdatePluginOutcome::NotInstalled));
    assert_eq!(bytes(&config), before);
    assert!(!home.path().join("plugins/tracedecay").exists());
}

#[test]
fn codex_update_plugin_stages_source_and_preserves_native_cache_and_config() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    let cache = codex_cached_plugin_dir(home.path()).join("native-cache.txt");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let manifest = cache.parent().unwrap().join(".codex-plugin/plugin.json");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(&manifest, r#"{"name":"tracedecay"}"#).unwrap();
    std::fs::write(&cache, "cache-owned-by-codex\n").unwrap();
    let config = home.path().join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "model = \"user-choice\"\n").unwrap();
    let cache_before = bytes(&cache);
    let config_before = bytes(&config);

    let outcome = get_integration("codex")
        .unwrap()
        .update_plugin(&ctx(home.path(), NEW_BIN))
        .unwrap();
    let UpdatePluginOutcome::DeferredUserAction(deferred) = outcome else {
        panic!("Codex cache refresh must be deferred to Codex");
    };
    assert!(
        deferred
            .remediation
            .contains("codex plugin update tracedecay@personal")
    );
    assert_eq!(bytes(&cache), cache_before);
    assert_eq!(bytes(&config), config_before);
    assert_codex_bundle_contains_bin(
        &codex_bootstrap_dir(home.path()),
        NEW_BIN,
        CodexScope::Global,
    );
}

// ---------------------------------------------------------------------------
// Kiro
// ---------------------------------------------------------------------------

#[test]
fn kiro_update_plugin_rebakes_managed_agent_and_preserves_configs() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
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
    let _agent_env = AgentEnvLock::pin(&home);
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
// Kimi
// ---------------------------------------------------------------------------

#[test]
fn kimi_update_plugin_stages_bundle_and_preserves_official_host_state() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    let kimi_code_home = home.path().join("kimi-code-home");
    let _kimi_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        &kimi_code_home,
    );
    let kimi = get_integration("kimi").unwrap();

    std::fs::create_dir_all(&kimi_code_home).unwrap();
    let user_mcp = kimi_code_home.join("mcp.json");
    std::fs::write(
        &user_mcp,
        "{\n  \"mcpServers\": {\n    \"other\": { \"command\": \"other-bin\" }\n  }\n}\n",
    )
    .unwrap();
    let user_mcp_before = bytes(&user_mcp);

    let installed_path = kimi_code_home.join("plugins/installed.json");
    std::fs::create_dir_all(installed_path.parent().unwrap()).unwrap();
    let installed = json!({
        "version": 1,
        "plugins": [{
            "id": "tracedecay",
            "enabled": false,
            "installedAt": "2020-01-01T00:00:00Z"
        }]
    });
    std::fs::write(
        &installed_path,
        serde_json::to_string_pretty(&installed).unwrap(),
    )
    .unwrap();
    let registry_before = bytes(&installed_path);

    let outcome = kimi
        .update_plugin(&ctx(home.path(), NEW_BIN))
        .expect("Kimi maintenance should return a typed deferral");
    let UpdatePluginOutcome::DeferredUserAction(deferred) = outcome else {
        panic!("Kimi maintenance should require deferred user action");
    };
    assert_eq!(bytes(&user_mcp), user_mcp_before);
    assert_eq!(bytes(&installed_path), registry_before);
    assert!(!kimi_code_home.join("plugins/managed/tracedecay").exists());

    let staged = home
        .path()
        .join(".tracedecay/host-bundle-stage/kimi/tracedecay");
    assert_eq!(deferred.staged_paths, vec![staged.clone()]);
    assert!(
        deferred
            .remediation
            .contains("interactive `/plugins` host API")
    );
    assert!(
        deferred
            .remediation
            .contains(&format!("/plugins install {}", staged.display()))
    );
    assert!(
        deferred
            .remediation
            .contains("made no current plugin registration changes")
    );
    let manifest = text(&staged.join(".kimi-plugin/plugin.json"));
    assert!(manifest.contains(NEW_BIN));
    assert!(manifest.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn kimi_update_plugin_reports_not_installed_without_a_plugin() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    let _kimi_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        home.path().join("kimi-code-home"),
    );
    let kimi = get_integration("kimi").unwrap();
    let outcome = kimi.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::NotInstalled));
    assert!(
        !home
            .path()
            .join("kimi-code-home/plugins/managed/tracedecay")
            .exists()
    );
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
        "gemini",
        "copilot",
        "zed",
        "cline",
        "roo-code",
        "antigravity",
        "kilo",
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
            relative_files_under(home.path()).is_empty(),
            "{id} update_plugin wrote files into the home dir"
        );
    }
}

#[test]
fn opencode_update_plugin_reports_not_installed_without_a_plugin() {
    // OpenCode ships a real plugin file since the PR13 host components, so it
    // is no longer config-only; without an installed plugin the update is a
    // no-op that must write nothing.
    let home = TempDir::new().unwrap();
    let agent = get_integration("opencode").unwrap();
    let outcome = agent.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::NotInstalled));
    assert!(
        relative_files_under(home.path()).is_empty(),
        "opencode update_plugin wrote files into the home dir"
    );
}

fn staged_host_source(host: &str) -> TempDir {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin");
    let staged = TempDir::new().expect("temp host source");
    let mut copies: Vec<(String, String)> = Vec::new();
    for name in subdir_names(&src.join("skills")) {
        if host == "cursor" && name.starts_with("tracedecay-") {
            continue;
        }
        let rel = format!("skills/{name}/SKILL.md");
        copies.push((rel.clone(), rel));
    }
    match host {
        "cursor" => {
            for entry in std::fs::read_dir(src.join("overlays/cursor/commands")).unwrap() {
                let file = entry.unwrap().file_name().to_string_lossy().into_owned();
                copies.push((
                    format!("overlays/cursor/commands/{file}"),
                    format!("commands/{file}"),
                ));
            }
            copies.push((
                ".cursor-plugin/plugin.json".into(),
                ".cursor-plugin/plugin.json".into(),
            ));
            copies.push(("mcp-cursor.json".into(), "mcp.json".into()));
            copies.push(("hooks/hooks-cursor.json".into(), "hooks/hooks.json".into()));
            copies.push(("README-cursor.md".into(), "README.md".into()));
            copies.push(("rules/tracedecay.mdc".into(), "rules/tracedecay.mdc".into()));
        }
        "codex" => {
            copies.push((
                ".codex-plugin/plugin.json".into(),
                ".codex-plugin/plugin.json".into(),
            ));
            copies.push((".mcp.json".into(), ".mcp.json".into()));
            copies.push(("hooks/hooks-codex.json".into(), "hooks/hooks.json".into()));
            copies.push(("README-codex.md".into(), "README.md".into()));
        }
        other => panic!("unknown host {other} (only cursor/codex are staged)"),
    }
    for (source, deploy) in copies {
        let target = staged.path().join(&deploy);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(src.join(&source), &target)
            .unwrap_or_else(|e| panic!("copy {source} -> {deploy}: {e}"));
    }
    if host == "cursor" {
        for (relative, contents) in tracedecay::agents::plugin_bundle::cursor_files()
            .into_iter()
            .filter(|(relative, _)| relative.starts_with("agents/"))
        {
            let target = staged.path().join(relative);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
    }
    staged
}

fn subdir_names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .unwrap_or_else(|e| panic!("read {}: {e}", root.display()))
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A vendored Cursor schema under `tests/fixtures/cursor-schemas/`, loaded by
/// path so the fixtures stay the single source of truth for manifest shape.
fn load_vendored_schema(file_name: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cursor-schemas")
        .join(file_name);
    read_json(&path)
}

/// The exact shell-quoted form the renderer bakes into hook commands:
/// single-quoted on POSIX, double-quoted on Windows (matching
/// `agents::hook_command`, which the test binary's platform selects).
fn quoted_bin(bin: &str) -> String {
    if cfg!(windows) {
        format!("\"{bin}\"")
    } else {
        format!("'{bin}'")
    }
}

/// A rendered hook command must be exactly `<quoted absolute bin> <hook
/// subcommand>` — quoting guards paths with spaces, and an absolute path
/// guards against PATH-dependent hooks.
fn assert_rendered_hook_command(command: &str, bin: &str, subcommand_prefix: &str) {
    assert!(
        Path::new(bin).has_root(),
        "hook binary path {bin:?} must be absolute"
    );
    let quoted = quoted_bin(bin);
    let suffix = command.strip_prefix(&quoted).unwrap_or_else(|| {
        panic!("hook command {command:?} must start with the quoted binary path {quoted:?}")
    });
    let subcommand = suffix.strip_prefix(' ').unwrap_or_else(|| {
        panic!("hook command {command:?} must separate binary and subcommand with a space")
    });
    assert!(
        subcommand.starts_with(subcommand_prefix) && !subcommand.trim().is_empty(),
        "hook command {command:?} must invoke a {subcommand_prefix}* subcommand"
    );
}

/// Collects every string value containing a `${...}` placeholder from the
/// rendered JSON files under `install_dir`, as
/// `(relative file, JSON pointer, value)`. Only JSON files are scanned:
/// they are the rendered config surfaces, while markdown (README, skills)
/// legitimately documents the placeholder syntax.
fn rendered_json_placeholders(install_dir: &Path) -> Vec<(String, String, String)> {
    fn walk(value: &serde_json::Value, pointer: &str, out: &mut Vec<(String, String)>) {
        match value {
            serde_json::Value::String(s) if s.contains("${") => {
                out.push((pointer.to_string(), s.clone()));
            }
            serde_json::Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    walk(item, &format!("{pointer}/{index}"), out);
                }
            }
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    walk(item, &format!("{pointer}/{key}"), out);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    for relative in relative_files_under(install_dir) {
        if relative.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let file = relative.to_string_lossy().replace('\\', "/");
        let mut in_file = Vec::new();
        walk(&read_json(&install_dir.join(&relative)), "", &mut in_file);
        found.extend(
            in_file
                .into_iter()
                .map(|(pointer, value)| (file.clone(), pointer, value)),
        );
    }
    found
}

/// Every file shipped in the source bundle must appear in the rendered
/// install — a renderer that skips a file drops it silently, because install
/// wipes the previous managed files first. The rendered dir may hold extras
/// (managed skill overlay, user files); source ⊆ rendered is the contract.
fn assert_source_bundle_fully_rendered(source_dir: &Path, install_dir: &Path) {
    let source = relative_files_under(source_dir);
    assert!(
        !source.is_empty(),
        "source bundle {} should not be empty",
        source_dir.display()
    );
    let rendered = relative_files_under(install_dir);
    let missing: Vec<&PathBuf> = source
        .iter()
        .filter(|relative| !rendered.contains(relative))
        .collect();
    assert!(
        missing.is_empty(),
        "files present in {} but missing from rendered install {}: {missing:?}",
        source_dir.display(),
        install_dir.display()
    );
}

/// Full structural validation of a rendered Cursor plugin bundle.
fn assert_cursor_rendered_bundle_valid(plugin_dir: &Path, bin: &str) {
    // Rendered mcp.json: absolute command, and the args pin — `serve --path
    // ${workspaceFolder}` is the one placeholder that must survive rendering.
    // Normal Cursor windows expand the variable at session start; hosts that
    // pass it through literally (headless agent-session scopes) are handled
    // by serve's unexpanded-template fallback, not by dropping the argument
    // from the template.
    let mcp = read_json(&plugin_dir.join("mcp.json"));
    let server = &mcp["mcpServers"]["tracedecay"];
    assert_eq!(server["type"], "stdio");
    assert_eq!(server["command"], json!(bin));
    assert!(
        Path::new(bin).has_root(),
        "rendered MCP command {bin:?} must be an absolute path"
    );
    assert_eq!(
        server["args"],
        json!(["serve", "--path", "${workspaceFolder}"]),
        "rendered mcp.json args must keep the workspaceFolder placeholder pin"
    );

    // Rendered manifest: still valid against the vendored official plugin
    // schema, with the version stamped to this binary's package version.
    let manifest_path = plugin_dir.join(".cursor-plugin/plugin.json");
    let manifest = read_json(&manifest_path);
    let plugin_schema = compile_schema(&load_vendored_schema("plugin.schema.json"));
    assert_schema_valid(&plugin_schema, &manifest, &manifest_path);
    assert_eq!(manifest["name"], "tracedecay");
    assert_eq!(
        manifest["version"],
        json!(env!("CARGO_PKG_VERSION")),
        "rendered manifest version must match the binary's package version"
    );

    // Rendered hooks.json: every event handler runs the quoted absolute
    // binary with a hook-cursor-* subcommand.
    let hooks = read_json(&plugin_dir.join("hooks/hooks.json"));
    let events = hooks["hooks"]
        .as_object()
        .expect("rendered hooks.json must contain a hooks object");
    assert!(
        !events.is_empty(),
        "rendered hooks.json must register events"
    );
    for (event, entries) in events {
        let entries = entries
            .as_array()
            .unwrap_or_else(|| panic!("hook event {event} must hold an array"));
        assert!(!entries.is_empty(), "hook event {event} must not be empty");
        for entry in entries {
            let command = entry["command"]
                .as_str()
                .unwrap_or_else(|| panic!("hook event {event} entry must carry a command"));
            assert_rendered_hook_command(command, bin, "hook-cursor-");
        }
    }

    // No placeholder survives rendering anywhere else in the JSON surfaces.
    assert_eq!(
        rendered_json_placeholders(plugin_dir),
        vec![(
            "mcp.json".to_string(),
            "/mcpServers/tracedecay/args/2".to_string(),
            "${workspaceFolder}".to_string()
        )],
        "the mcp.json args pin is the only placeholder allowed in rendered JSON"
    );

    // Nothing from the source bundle was silently dropped.
    let staged = staged_host_source("cursor");
    assert_source_bundle_fully_rendered(staged.path(), plugin_dir);
}

/// Full structural validation of a rendered Codex plugin bundle.
fn assert_codex_rendered_bundle_valid(plugin_dir: &Path, bin: &str, scope: CodexScope) {
    // Rendered manifest: version stamped to this binary's package version.
    let manifest = read_json(&plugin_dir.join(".codex-plugin/plugin.json"));
    assert_eq!(manifest["name"], "tracedecay");
    assert_eq!(
        manifest["version"],
        json!(env!("CARGO_PKG_VERSION")),
        "rendered manifest version must match the binary's package version"
    );

    match scope {
        CodexScope::Global => {
            // Rendered hooks.json: Codex nests handlers in matcher groups;
            // every handler command is the quoted absolute binary plus a
            // hook-codex-* subcommand.
            let hooks = read_json(&plugin_dir.join("hooks/hooks.json"));
            let events = hooks["hooks"]
                .as_object()
                .expect("rendered hooks.json must contain a hooks object");
            assert!(
                !events.is_empty(),
                "rendered hooks.json must register events"
            );
            for (event, groups) in events {
                let groups = groups
                    .as_array()
                    .unwrap_or_else(|| panic!("hook event {event} must hold an array of groups"));
                assert!(!groups.is_empty(), "hook event {event} must not be empty");
                for group in groups {
                    let handlers = group["hooks"]
                        .as_array()
                        .unwrap_or_else(|| panic!("hook event {event} group must carry handlers"));
                    for handler in handlers {
                        let command = handler["command"].as_str().unwrap_or_else(|| {
                            panic!("hook event {event} handler must carry a command")
                        });
                        assert_rendered_hook_command(command, bin, "hook-codex-");
                    }
                }
            }
        }
        CodexScope::RepoLocal => {
            // Repo-local bundles get their lifecycle hooks from the global
            // plugin: no hooks file, no manifest hooks declaration.
            assert!(
                !plugin_dir.join("hooks/hooks.json").exists(),
                "repo-local Codex bundle must not ship lifecycle hooks"
            );
            assert!(
                manifest.get("hooks").is_none(),
                "repo-local Codex manifest must not declare lifecycle hooks"
            );
        }
    }

    // Codex has no intentional placeholder: nothing may survive rendering.
    assert_eq!(
        rendered_json_placeholders(plugin_dir),
        Vec::<(String, String, String)>::new(),
        "no placeholder may survive rendering in the Codex bundle"
    );

    // Nothing from the source bundle was silently dropped. Repo-local
    // bundles intentionally drop the hooks file, so remove it from the
    // staged expectation.
    let staged = staged_host_source("codex");
    if scope == CodexScope::RepoLocal {
        std::fs::remove_file(staged.path().join("hooks/hooks.json")).unwrap();
    }
    assert_source_bundle_fully_rendered(staged.path(), plugin_dir);
}

#[test]
fn cursor_install_renders_structurally_valid_bundle() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    let cursor = get_integration("cursor").unwrap();
    cursor.install(&ctx(home.path(), NEW_BIN)).unwrap();
    assert_cursor_rendered_bundle_valid(
        &home.path().join(".cursor/plugins/local/tracedecay"),
        NEW_BIN,
    );
}

#[test]
fn cursor_update_plugin_rerenders_structurally_valid_bundle() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    let cursor = get_integration("cursor").unwrap();
    cursor.install(&ctx(home.path(), OLD_BIN)).unwrap();

    let outcome = cursor.update_plugin(&ctx(home.path(), NEW_BIN)).unwrap();
    assert!(matches!(outcome, UpdatePluginOutcome::Refreshed(_)));
    assert_cursor_rendered_bundle_valid(
        &home.path().join(".cursor/plugins/local/tracedecay"),
        NEW_BIN,
    );
}

#[test]
fn codex_install_renders_structurally_valid_bundle() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    let codex = get_integration("codex").unwrap();
    codex.install(&ctx(home.path(), NEW_BIN)).unwrap();

    let plugin_dir = codex_bootstrap_dir(home.path());
    assert_codex_rendered_bundle_valid(&plugin_dir, NEW_BIN, CodexScope::Global);

    // Global-scope MCP rendering: absolute command, plain `serve` args, and
    // the global-DB env flag.
    let mcp = read_json(&plugin_dir.join(".mcp.json"));
    let server = &mcp["mcpServers"]["graph"];
    assert_eq!(server["type"], "stdio");
    assert_eq!(server["command"], json!(NEW_BIN));
    assert_eq!(server["args"], json!(["serve"]));
    assert_eq!(server["startup_timeout_sec"], json!(120));
    assert_eq!(server["tool_timeout_sec"], json!(900));
    assert_eq!(server["env"], json!({ "TRACEDECAY_ENABLE_GLOBAL_DB": "1" }));
}

#[test]
fn codex_local_install_renders_project_scoped_mcp() {
    let home = TempDir::new().unwrap();
    let _agent_env = AgentEnvLock::pin(&home);
    let project = TempDir::new().unwrap();
    let codex = get_integration("codex").unwrap();
    codex
        .install_local(&ctx(home.path(), NEW_BIN), project.path())
        .unwrap();

    let plugin_dir = codex_bootstrap_dir(project.path());
    assert_codex_rendered_bundle_valid(&plugin_dir, NEW_BIN, CodexScope::RepoLocal);

    // Project-local scope renders relative-path serve args and drops the
    // global-DB env flag.
    let mcp = read_json(&plugin_dir.join(".mcp.json"));
    let server = &mcp["mcpServers"]["graph"];
    assert_eq!(server["command"], json!(NEW_BIN));
    assert_eq!(server["args"], json!(["serve", "--path", "."]));
    assert_eq!(server["startup_timeout_sec"], json!(120));
    assert_eq!(server["tool_timeout_sec"], json!(900));
    assert!(
        server.get("env").is_none(),
        "project-local installs must not enable the global DB"
    );
}

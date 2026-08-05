//! Codex agent tests: plugin bundle/marketplace installs, hook bundling,
//! uninstall, and healthchecks.

use crate::agent_test_support::*;
use crate::common::{EnvVarGuard, PROCESS_ENV_LOCK as AGENT_ENV_LOCK};
use tempfile::TempDir;
use tracedecay::agents::*;
use tracedecay::automation::managed_skills::{approve_managed_skill, create_managed_skill_draft};
use tracedecay::config::USER_DATA_DIR_ENV;

#[test]
fn test_codex_install_stages_source_and_preserves_host_native_state() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let config_path = home.join(".codex/config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "model = \"user-choice\"\n").unwrap();
    let cache_path = home.join(".codex/plugins/cache/personal/tracedecay/1/user-cache.txt");
    std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    std::fs::write(&cache_path, "native cache bytes\n").unwrap();
    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    let plugin_dir = codex_plugin_install_dir(home);
    assert_codex_plugin_bundle(
        &plugin_dir,
        &ctx.tracedecay_bin,
        serde_json::json!(["serve"]),
        true,
    );
    assert_codex_personal_marketplace_entry(home);

    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        "model = \"user-choice\"\n"
    );
    assert_eq!(
        std::fs::read_to_string(&cache_path).unwrap(),
        "native cache bytes\n"
    );
    assert!(
        !home.join(".codex/hooks.json").exists(),
        "global Codex install should bundle hooks in the plugin, not write ~/.codex/hooks.json"
    );
    assert!(
        !home.join(".codex/AGENTS.md").exists(),
        "global Codex install should use plugin skills, not write ~/.codex/AGENTS.md"
    );
}

#[tokio::test]
async fn test_codex_install_exports_active_managed_skills() {
    let _env_lock = AGENT_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let profile_root = home.join(".tracedecay");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
    create_managed_skill_draft(
        &profile_root,
        managed_skill_draft("repo-hygiene", "Repo Hygiene"),
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();

    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    let skill_path =
        codex_plugin_install_dir(home).join("skills/agent-managed/repo-hygiene/SKILL.md");
    let skill = std::fs::read_to_string(skill_path).unwrap();
    assert!(skill.contains("name: repo-hygiene"));
    assert!(skill.contains("description:"));
    assert!(!skill.contains("id: repo-hygiene"));
    assert!(skill.contains("Use Repo Hygiene for repeated workflows."));
    let digest_skill_path =
        codex_plugin_install_dir(home).join("skills/agent-managed-memory/SKILL.md");
    assert!(
        !digest_skill_path.exists(),
        "Codex memory digest is delivered through hook additionalContext, not a duplicate skill"
    );
}

#[tokio::test]
async fn test_codex_shareable_plugin_artifact_exports_bundle_and_managed_skills() {
    let _env_lock = AGENT_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let profile_root = home.join(".tracedecay");
    create_managed_skill_draft(
        &profile_root,
        managed_skill_draft("repo-hygiene", "Repo Hygiene"),
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();

    let output = home.join("shareable-codex-plugin");
    let summary = tracedecay::agents::codex::export_codex_plugin_artifact(
        &profile_root,
        &output,
        "/usr/local/bin/tracedecay",
    )
    .unwrap();

    assert_eq!(summary.exported_count, 1);
    assert_eq!(summary.output, output);
    assert_codex_plugin_bundle(
        &summary.output,
        "/usr/local/bin/tracedecay",
        serde_json::json!(["serve"]),
        true,
    );
    let skill_path = summary
        .output
        .join("skills/agent-managed/repo-hygiene/SKILL.md");
    let skill = std::fs::read_to_string(skill_path).unwrap();
    assert!(skill.contains("name: repo-hygiene"));
    assert!(skill.contains("description:"));
    assert!(!skill.contains("id: repo-hygiene"));
    assert!(skill.contains("Use Repo Hygiene for repeated workflows."));
    assert!(
        !summary
            .output
            .join("skills/agent-managed-memory/SKILL.md")
            .exists(),
        "shareable Codex plugin artifacts must not include a personal memory digest"
    );
}

#[test]
fn test_codex_install_preserves_existing_marketplace_identity() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    write_codex_personal_marketplace(home, "my-marketplace", "My Marketplace");

    CodexIntegration.install(&ctx).unwrap();

    assert_codex_marketplace_entry(
        &codex_personal_marketplace_path(home),
        "my-marketplace",
        "My Marketplace",
        "./plugins/tracedecay",
    );
}

#[test]
fn test_codex_install_preserves_existing_global_codex_files() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let config_path = codex_dir.join("config.toml");
    let hooks_path = codex_dir.join("hooks.json");
    let agents_path = codex_dir.join("AGENTS.md");
    let config = "[mcp_servers.tracedecay]\ncommand = \"/old/tracedecay\"\nargs = [\"serve\"]\n";
    let hooks = r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"/old/tracedecay hook-codex-session-start","timeout":5}]}]}}"#;
    let agents = "## Prefer tracedecay MCP tools\n\nUse tracedecay.\n";
    std::fs::write(&config_path, config).unwrap();
    std::fs::write(&hooks_path, hooks).unwrap();
    std::fs::write(&agents_path, agents).unwrap();

    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    assert_eq!(std::fs::read_to_string(config_path).unwrap(), config);
    assert_eq!(std::fs::read_to_string(hooks_path).unwrap(), hooks);
    assert_eq!(std::fs::read_to_string(agents_path).unwrap(), agents);
    assert_codex_plugin_bundle(
        &codex_plugin_install_dir(home),
        &ctx.tracedecay_bin,
        serde_json::json!(["serve"]),
        true,
    );
}

#[test]
fn test_codex_local_install_creates_repo_plugin_bundle_and_marketplace() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join(".codex/agents")).unwrap();
    std::fs::write(
        home.path().join(".codex/agents/user-agent.toml"),
        "name = \"user-agent\"\n",
    )
    .unwrap();

    assert_local_install_success("codex", project.path(), home.path());

    let plugin_dir = codex_project_plugin_install_dir(project.path());
    assert_codex_plugin_bundle(
        &plugin_dir,
        &expected_tracedecay_bin(),
        serde_json::json!(["serve", "--path", "."]),
        false,
    );
    assert_codex_repo_marketplace_entry(project.path());
    assert!(
        !project.path().join(".codex/config.toml").exists(),
        "local Codex install should use the repo plugin marketplace, not write .codex/config.toml"
    );
    assert!(
        !project.path().join(".codex/hooks.json").exists(),
        "local Codex install should not write project Codex hooks"
    );
    assert!(
        !project.path().join("AGENTS.md").exists(),
        "local Codex install should use plugin skills, not write project AGENTS.md"
    );
    assert!(
        !home
            .path()
            .join(".codex/agents/tracedecay-code-explorer.toml")
            .exists(),
        "local Codex install must not write native Codex agent configuration"
    );
    assert!(
        home.path().join(".codex/agents/user-agent.toml").is_file(),
        "local Codex install must preserve foreign user agents"
    );
}

#[test]
fn test_codex_local_install_does_not_export_personal_memory_digest_into_repo() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    assert_local_install_success("codex", project.path(), home.path());

    let digest_skill_path = codex_project_plugin_install_dir(project.path())
        .join("skills/agent-managed-memory/SKILL.md");
    assert!(
        !digest_skill_path.exists(),
        "project-local Codex install must not export the personal memory digest into the repo"
    );

    let targets_path = home
        .path()
        .join(".tracedecay/agent_managed/memory_digest_targets.json");
    if targets_path.exists() {
        let targets = std::fs::read_to_string(&targets_path).unwrap();
        assert!(
            !targets.contains(&project.path().display().to_string()),
            "project-local Codex install must not record repo-tree digest targets"
        );
    }
}

#[test]
fn test_codex_local_install_preserves_existing_project_codex_files() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let codex_dir = project.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let config_path = codex_dir.join("config.toml");
    let hooks_path = codex_dir.join("hooks.json");
    let config = "[mcp_servers.tracedecay]\ncommand = \"/old/tracedecay\"\nargs = [\"serve\", \"--path\", \".\"]\n";
    let hooks = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/old/tracedecay hook-codex-pre-tool-use","timeout":5}]}],"PostToolUse":[{"matcher":"Bash|apply_patch","hooks":[{"type":"command","command":"/old/tracedecay hook-codex-post-tool-use","timeout":60}]}]}}"#;
    std::fs::write(&config_path, config).unwrap();
    std::fs::write(&hooks_path, hooks).unwrap();

    assert_local_install_success("codex", project.path(), home.path());

    assert_eq!(std::fs::read_to_string(config_path).unwrap(), config);
    assert_eq!(std::fs::read_to_string(hooks_path).unwrap(), hooks);
    assert_codex_plugin_bundle(
        &codex_project_plugin_install_dir(project.path()),
        &expected_tracedecay_bin(),
        serde_json::json!(["serve", "--path", "."]),
        false,
    );
}

#[test]
fn test_codex_global_install_bundles_hooks_in_plugin() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    let hooks_path = codex_plugin_install_dir(home).join("hooks/hooks.json");
    assert!(
        hooks_path.exists(),
        "global Codex install should bundle hooks in the plugin"
    );
    let hooks = read_json(&hooks_path);
    assert_codex_hooks_registered(&hooks);
    assert!(
        !home.join(".codex/hooks.json").exists(),
        "global Codex install should not write hooks outside the plugin"
    );
}

#[test]
fn test_codex_local_install_does_not_bundle_hooks() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    assert_local_install_success("codex", project.path(), home.path());

    let hooks_path = codex_project_plugin_install_dir(project.path()).join("hooks/hooks.json");
    assert!(
        !hooks_path.exists(),
        "local Codex install should not bundle project-local hooks"
    );
    assert!(
        !home.path().join(".codex/hooks.json").exists(),
        "local install must not write the global Codex hooks config"
    );
    assert!(
        !project.path().join(".codex/hooks.json").exists(),
        "local install must not write project Codex hooks config"
    );
}

#[test]
fn test_codex_install_reconciles_hooks_idempotently() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);

    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();
    CodexIntegration.install(&ctx).unwrap();

    let hooks = read_json(&codex_plugin_install_dir(home).join("hooks/hooks.json"));
    let groups = hooks["hooks"]["PostToolUse"].as_array().unwrap();

    let tracedecay_groups: Vec<_> = groups
        .iter()
        .filter(|group| {
            group["hooks"].as_array().is_some_and(|handlers| {
                handlers.iter().any(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|c| c.contains("hook-codex-post-tool-use"))
                })
            })
        })
        .collect();
    assert_eq!(
        tracedecay_groups.len(),
        1,
        "reinstall must keep exactly one tracedecay PostToolUse group, got {groups:?}"
    );
    assert_codex_hooks_registered(&hooks);
}

#[test]
fn test_codex_global_uninstall_defers_to_native_cli_without_tearing_down_source() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    CodexIntegration.install(&ctx).unwrap();
    let hooks_path = codex_plugin_install_dir(home).join("hooks/hooks.json");
    assert!(hooks_path.exists());

    let error = CodexIntegration.uninstall(&ctx).unwrap_err().to_string();
    assert!(error.contains("codex plugin remove tracedecay@personal"));
    assert!(
        hooks_path.exists(),
        "global uninstall must not remove source until Codex has removed its native plugin"
    );
}

#[test]
fn test_codex_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let config_path = home.join(".codex/config.toml");
    let original = "\
model = \"o4-mini\"
approval_policy = \"on-failure\"

[mcp_servers.other]
command = \"other-bin\"
args = [\"--flag\"]
";
    std::fs::write(&config_path, original).unwrap();

    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
    assert!(!home.join(".codex/config.toml.bak").exists());
    assert_codex_plugin_bundle(
        &codex_plugin_install_dir(home),
        &ctx.tracedecay_bin,
        serde_json::json!(["serve"]),
        true,
    );
}

#[test]
fn test_codex_install_leaves_unparseable_config_untouched() {
    // Global Codex installs no longer rewrite config.toml, so even a broken
    // user config must be left byte-identical while the plugin bundle installs.
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let config_path = home.join(".codex/config.toml");
    let original = "this is not valid TOML {{{{";
    std::fs::write(&config_path, original).unwrap();

    let ctx = make_install_ctx(home);
    let result = CodexIntegration.install(&ctx);
    assert!(
        result.is_ok(),
        "global Codex plugin install should not parse or rewrite config.toml"
    );
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        original,
        "the broken config must be left untouched so the user can fix it"
    );
}

#[test]
fn test_healthcheck_codex_after_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    CodexIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.issues, 0,
        "Codex healthcheck should pass after a clean install"
    );
}

#[test]
fn test_healthcheck_codex_local_install_checks_project_config() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    assert_local_install_success("codex", project.path(), home.path());

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    CodexIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.issues, 0,
        "local Codex healthcheck should pass without global ~/.codex config"
    );
}

#[test]
fn test_healthcheck_codex_local_install_warns_when_repo_marketplace_missing() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    assert_local_install_success("codex", project.path(), home.path());
    std::fs::remove_file(codex_repo_marketplace_path(project.path())).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    CodexIntegration.healthcheck(&mut dc, &hctx);
    assert!(
        dc.issues > 0 || dc.warnings > 0,
        "local Codex healthcheck should flag a missing repo plugin marketplace"
    );
}

#[test]
fn test_healthcheck_codex_ignores_unrelated_project_agents_md() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("AGENTS.md"),
        "Project-specific agent instructions without tracedecay.\n",
    )
    .unwrap();
    let ctx = make_install_ctx(home.path());
    CodexIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    CodexIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.issues, 0,
        "global Codex healthcheck should be used when project AGENTS.md is unrelated"
    );
}

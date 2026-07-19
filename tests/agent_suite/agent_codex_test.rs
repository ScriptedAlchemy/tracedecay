//! Codex agent tests: plugin bundle/marketplace installs, hook bundling,
//! uninstall, and healthchecks.

use crate::agent_test_support::*;
use crate::common::{EnvVarGuard, PROCESS_ENV_LOCK as AGENT_ENV_LOCK};
use tempfile::TempDir;
use tracedecay::agents::*;
use tracedecay::automation::managed_skills::{
    SkillInstallTarget, approve_managed_skill, create_managed_skill_draft,
};
use tracedecay::config::USER_DATA_DIR_ENV;

#[test]
fn test_codex_install_creates_plugin_bundle_and_marketplace() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
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

    // Global Codex install auto-trusts the bundled lifecycle hooks by writing
    // their content hashes into ~/.codex/config.toml, so Codex runs them without
    // a manual /hooks approval. The MCP server itself still comes from the
    // plugin bundle, not config.toml.
    let config = std::fs::read_to_string(home.join(".codex/config.toml"))
        .expect("global Codex install should record hook trust in config.toml");
    assert!(
        config.contains("tracedecay@personal:hooks/hooks.json:") && config.contains("trusted_hash"),
        "global Codex install should record tracedecay hook trust entries, got:\n{config}"
    );
    assert!(
        !config.contains("[mcp_servers.tracedecay]"),
        "global Codex install should not register the MCP server in config.toml"
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
    assert!(
        home.join(".codex/agents/tracedecay-code-explorer.toml")
            .is_file(),
        "Codex install should write TraceDecay custom agents to ~/.codex/agents"
    );

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
fn test_codex_install_refreshes_existing_cache_and_keeps_bootstrap_source_listable() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    let stale_plugin_dir = codex_stale_cached_plugin_install_dir(home);
    write_codex_plugin_manifest(&stale_plugin_dir, "0.0.0");

    let bootstrap_dir = codex_plugin_install_dir(home);
    write_codex_plugin_manifest(&bootstrap_dir, "0.0.0");
    write_stale_codex_skill(&bootstrap_dir);
    write_codex_personal_marketplace(home, "personal", "Personal");

    CodexIntegration.install(&ctx).unwrap();

    let cached_plugin_dir = codex_cached_plugin_install_dir(home);
    assert_codex_plugin_bundle(
        &cached_plugin_dir,
        &ctx.tracedecay_bin,
        serde_json::json!(["serve"]),
        true,
    );
    assert_codex_plugin_bundle(
        &bootstrap_dir,
        &ctx.tracedecay_bin,
        serde_json::json!(["serve"]),
        true,
    );
    assert!(
        !bootstrap_dir.join("skills/stale-skill/SKILL.md").exists(),
        "global Codex install should refresh the bootstrap source so plugin list/add sees current skills"
    );
    assert!(
        !stale_plugin_dir.exists(),
        "global Codex install should migrate managed cache installs to the current plugin version"
    );
    assert_codex_personal_marketplace_entry(home);
}

#[test]
fn test_codex_install_migrates_legacy_caveman_home_cache_and_marketplace() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    let legacy_plugin_dir = codex_legacy_cached_plugin_install_dir(home);
    write_codex_plugin_manifest(&legacy_plugin_dir, "0.0.0");
    write_stale_codex_skill(&legacy_plugin_dir);
    write_codex_personal_marketplace(home, "caveman-home", "Caveman Home");

    CodexIntegration.install(&ctx).unwrap();

    let cached_plugin_dir = codex_cached_plugin_install_dir(home);
    assert_codex_plugin_bundle(
        &cached_plugin_dir,
        &ctx.tracedecay_bin,
        serde_json::json!(["serve"]),
        true,
    );
    assert!(
        !legacy_plugin_dir.exists(),
        "global Codex install should migrate legacy caveman-home cache installs to personal"
    );
    assert_codex_personal_marketplace_entry(home);
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
fn test_codex_install_refreshes_existing_cache_and_prunes_stale_skills() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    let cached_plugin_dir = codex_cached_plugin_install_dir(home);
    let bootstrap_dir = codex_plugin_install_dir(home);
    write_codex_plugin_manifest(&cached_plugin_dir, "0.0.0");
    write_stale_codex_skill(&cached_plugin_dir);
    std::fs::write(cached_plugin_dir.join("user-note.txt"), "mine\n").unwrap();

    CodexIntegration.install(&ctx).unwrap();

    assert_codex_plugin_bundle(
        &cached_plugin_dir,
        &ctx.tracedecay_bin,
        serde_json::json!(["serve"]),
        true,
    );
    assert_codex_plugin_bundle(
        &bootstrap_dir,
        &ctx.tracedecay_bin,
        serde_json::json!(["serve"]),
        true,
    );
    assert!(
        !cached_plugin_dir
            .join("skills/stale-skill/SKILL.md")
            .exists(),
        "refreshing an installed Codex plugin cache must prune obsolete managed skills"
    );
    assert_eq!(
        std::fs::read_to_string(cached_plugin_dir.join("user-note.txt")).unwrap(),
        "mine\n",
        "refresh should preserve unmanaged root-level user files"
    );
    assert_codex_personal_marketplace_entry(home);
}

#[test]
fn test_codex_install_sweeps_legacy_global_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        "[mcp_servers.tracedecay]\ncommand = \"/old/tracedecay\"\nargs = [\"serve\"]\n",
    )
    .unwrap();
    std::fs::write(
        codex_dir.join("hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"/old/tracedecay hook-codex-session-start","timeout":5}]}]}}"#,
    )
    .unwrap();
    std::fs::write(
        codex_dir.join("AGENTS.md"),
        "## Prefer tracedecay MCP tools\n\nUse tracedecay.\n",
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    // The legacy MCP registration is swept, and the installer records hook
    // trust in its place — so config.toml survives with only [hooks.state].
    let migrated: toml::Value =
        toml::from_str(&std::fs::read_to_string(codex_dir.join("config.toml")).unwrap()).unwrap();
    assert!(
        migrated
            .get("mcp_servers")
            .and_then(|servers| servers.get("tracedecay"))
            .is_none(),
        "legacy global Codex MCP config should be removed when it only contained tracedecay"
    );
    assert!(
        migrated["hooks"]["state"]
            .as_table()
            .unwrap()
            .keys()
            .any(|key| key.starts_with("tracedecay@personal:hooks/hooks.json:")),
        "install should record tracedecay hook trust in config.toml"
    );
    assert!(
        !codex_dir.join("hooks.json").exists(),
        "legacy global Codex hooks should be removed when they only contained tracedecay"
    );
    assert!(
        !codex_dir.join("AGENTS.md").exists(),
        "legacy global Codex prompt rules should be removed when they only contained tracedecay"
    );
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
        home.path()
            .join(".codex/agents/tracedecay-code-explorer.toml")
            .is_file(),
        "local Codex install must materialize managed agents in the user profile"
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
fn test_codex_local_install_sweeps_legacy_project_config() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let codex_dir = project.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        "[mcp_servers.tracedecay]\ncommand = \"/old/tracedecay\"\nargs = [\"serve\", \"--path\", \".\"]\n",
    )
    .unwrap();
    std::fs::write(
        codex_dir.join("hooks.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/old/tracedecay hook-codex-pre-tool-use","timeout":5}]}],"PostToolUse":[{"matcher":"Bash|apply_patch","hooks":[{"type":"command","command":"/old/tracedecay hook-codex-post-tool-use","timeout":60}]}]}}"#,
    )
    .unwrap();

    assert_local_install_success("codex", project.path(), home.path());

    assert!(
        !codex_dir.join("config.toml").exists(),
        "legacy project Codex MCP config should be removed when it only contained tracedecay"
    );
    assert!(
        !codex_dir.join("hooks.json").exists(),
        "legacy project Codex hooks should be removed when they only contained tracedecay"
    );
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
fn test_codex_uninstall_removes_plugin_hooks() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    CodexIntegration.install(&ctx).unwrap();
    let hooks_path = codex_plugin_install_dir(home).join("hooks/hooks.json");
    assert!(hooks_path.exists());

    CodexIntegration.uninstall(&ctx).unwrap();

    assert!(
        !hooks_path.exists(),
        "uninstall should remove tracedecay Codex plugin hooks with the plugin bundle"
    );
}

#[test]
fn test_codex_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    CodexIntegration.install(&ctx).unwrap();
    let plugin_dir = codex_plugin_install_dir(home);
    assert!(plugin_dir.exists());
    assert_codex_personal_marketplace_entry(home);
    let legacy_digest = plugin_dir.join("skills/agent-managed-memory/SKILL.md");
    std::fs::create_dir_all(legacy_digest.parent().unwrap()).unwrap();
    std::fs::write(&legacy_digest, "legacy digest").unwrap();
    seed_memory_digest_target(
        &home.join(".tracedecay"),
        tracedecay::automation::skill_targets::SkillInstallTarget::Codex,
        &plugin_dir,
    );

    CodexIntegration.uninstall(&ctx).unwrap();

    assert!(
        !plugin_dir.exists(),
        "Codex plugin bundle should be removed on uninstall"
    );
    let marketplace = read_json(&codex_personal_marketplace_path(home));
    assert!(
        marketplace["plugins"]
            .as_array()
            .is_none_or(|plugins| plugins.iter().all(|entry| entry["name"] != "tracedecay")),
        "Codex marketplace entry should be removed on uninstall"
    );

    let agents_md = home.join(".codex/AGENTS.md");
    if agents_md.exists() {
        let content = std::fs::read_to_string(&agents_md).unwrap();
        assert!(
            !content.contains("## Prefer tracedecay MCP tools"),
            "AGENTS.md should not have tracedecay rules after uninstall"
        );
    }

    std::fs::create_dir_all(&plugin_dir).unwrap();
    tracedecay::automation::memory_digest::export_memory_digest_to_recorded_targets(
        &home.join(".tracedecay"),
    )
    .unwrap();
    assert!(
        !plugin_dir
            .join("skills/agent-managed-memory/SKILL.md")
            .exists(),
        "Codex uninstall must unrecord memory digest targets so refresh cannot recreate them"
    );
}

#[test]
fn test_codex_local_uninstall_unrecords_legacy_repo_memory_digest_target() {
    let _env_lock = AGENT_ENV_LOCK.blocking_lock();
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let mut ctx = make_install_ctx(home.path());
    ctx.project_root = Some(project.path().to_path_buf());
    let profile_root = home.path().join(".tracedecay");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

    CodexIntegration
        .install_local(&ctx, project.path())
        .unwrap();

    let plugin_dir = codex_project_plugin_install_dir(project.path());
    let legacy_digest = plugin_dir.join("skills/agent-managed-memory/SKILL.md");
    std::fs::create_dir_all(legacy_digest.parent().unwrap()).unwrap();
    std::fs::write(&legacy_digest, "legacy digest").unwrap();
    seed_memory_digest_target(
        &profile_root,
        tracedecay::automation::skill_targets::SkillInstallTarget::Codex,
        &plugin_dir,
    );
    assert!(legacy_digest.exists());

    CodexIntegration.uninstall(&ctx).unwrap();

    std::fs::create_dir_all(&plugin_dir).unwrap();
    tracedecay::automation::memory_digest::export_memory_digest_to_recorded_targets(&profile_root)
        .unwrap();
    assert!(
        !plugin_dir
            .join("skills/agent-managed-memory/SKILL.md")
            .exists(),
        "project-local Codex uninstall must unrecord legacy repo-tree memory digest targets"
    );
}

#[test]
fn test_codex_install_preserves_existing_config() {
    // Regression test for issue #63: installing tracedecay used to wipe out the
    // entire ~/.codex/config.toml. The installer now records hook trust in
    // config.toml, so it must add only its [hooks.state] entries while
    // preserving every unrelated key the user already had, and back the file up
    // first (issue #63) before rewriting it.
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

    let updated: toml::Value =
        toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    // Unrelated user content survives untouched.
    assert_eq!(updated["model"].as_str().unwrap(), "o4-mini");
    assert_eq!(updated["approval_policy"].as_str().unwrap(), "on-failure");
    assert_eq!(
        updated["mcp_servers"]["other"]["command"].as_str().unwrap(),
        "other-bin"
    );
    // Hook trust entries were added for the personal plugin bundle.
    assert!(
        updated["hooks"]["state"]
            .as_table()
            .unwrap()
            .keys()
            .any(|key| key.starts_with("tracedecay@personal:hooks/hooks.json:")),
        "install should record tracedecay hook trust entries"
    );
    // Issue #63: the pre-existing config is backed up before the rewrite.
    assert_eq!(
        std::fs::read_to_string(home.join(".codex/config.toml.bak")).unwrap(),
        original,
        "install should back up the original config before rewriting it"
    );
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

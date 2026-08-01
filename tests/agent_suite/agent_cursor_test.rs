//! Cursor agent tests: plugin bundles, installs, legacy artifact sweeps,
//! symlink containment, and healthchecks.

use std::path::Path;
use std::process::Command;

use crate::agent_test_support::*;
use crate::common::{EnvVarGuard, PROCESS_ENV_LOCK as AGENT_ENV_LOCK};
use tempfile::TempDir;
use tracedecay::agents::*;
use tracedecay::automation::managed_skills::{
    SkillInstallTarget, approve_managed_skill, create_managed_skill_draft,
};
use tracedecay::branch_meta;
use tracedecay::config::USER_DATA_DIR_ENV;
use tracedecay::storage::resolve_layout_for_current_profile;

#[test]
fn test_cursor_plugin_bundle_files_are_valid() {
    // The single shared `plugin/` tree stores Cursor's files under host-specific
    // source names (e.g. `mcp-cursor.json`, `README-cursor.md`,
    // `hooks/hooks-cursor.json`). Stage them into a temp dir under their
    // *deploy* names so the un-rendered (`command: tracedecay`, `version:
    // 0.0.0`) source is validated exactly as it lands on disk pre-render.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin");
    let staged = TempDir::new().unwrap();
    let dst = staged.path();
    let copies: &[(&str, &str)] = &[
        (".cursor-plugin/plugin.json", ".cursor-plugin/plugin.json"),
        ("mcp-cursor.json", "mcp.json"),
        ("hooks/hooks-cursor.json", "hooks/hooks.json"),
        ("rules/tracedecay.mdc", "rules/tracedecay.mdc"),
        ("README-cursor.md", "README.md"),
    ];
    for (source, deploy) in copies {
        let target = dst.join(deploy);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(src.join(source), &target).unwrap();
    }
    assert_cursor_plugin_bundle(dst, "tracedecay", "0.0.0");
}

#[test]
fn test_cursor_install_installs_local_plugin_without_global_mcp() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);
    let ctx = make_install_ctx(home.path());

    CursorIntegration.install(&ctx).unwrap();

    let plugin_dir = cursor_plugin_install_dir(home.path());
    assert_cursor_plugin_bundle(&plugin_dir, &ctx.tracedecay_bin, env!("CARGO_PKG_VERSION"));
    assert!(
        !std::fs::symlink_metadata(&plugin_dir)
            .unwrap()
            .file_type()
            .is_symlink(),
        "Cursor install should write a real plugin directory, not a symlink"
    );
    assert!(
        !home.path().join(".cursor/mcp.json").exists(),
        "Cursor plugin install should not write legacy ~/.cursor/mcp.json"
    );
}

#[test]
fn test_cursor_plugin_hooks_quote_binary_paths_with_spaces() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);
    let tracedecay_bin = home.path().join("bin with spaces/tracedecay");
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: tracedecay_bin.to_string_lossy().to_string(),
        tool_permissions: expected_tool_perms(),
        project_root: None,
        dashboard: false,
    };

    CursorIntegration.install(&ctx).unwrap();

    let hooks = read_json(&cursor_plugin_install_dir(home.path()).join("hooks/hooks.json"));
    let command = hooks["hooks"]["sessionStart"][0]["command"]
        .as_str()
        .expect("sessionStart command should be a string");
    let expected_bin = tracedecay_bin.to_string_lossy();
    let expected = if cfg!(windows) {
        format!(
            "\"{}\" hook-cursor-session-start",
            expected_bin.replace('\\', "/")
        )
    } else {
        format!("'{}' hook-cursor-session-start", expected_bin)
    };
    assert_eq!(command, expected);
}

#[test]
fn test_local_install_cursor_installs_plugin_without_project_config() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    assert_local_install_success("cursor", project.path(), home.path());

    assert_cursor_plugin_bundle(
        &cursor_plugin_install_dir(home.path()),
        &expected_tracedecay_bin(),
        env!("CARGO_PKG_VERSION"),
    );

    let mcp_path = project.path().join(".cursor/mcp.json");
    assert!(
        !mcp_path.exists(),
        "Cursor local install should not write legacy project MCP config"
    );
    assert!(
        !project.path().join(".cursor/hooks.json").exists(),
        "Cursor local install should not write legacy project hooks"
    );
    assert!(
        !project.path().join(".cursor/rules/tracedecay.mdc").exists(),
        "Cursor local install should not write legacy project rule"
    );
    assert!(
        !project.path().join(".cursor/permissions.json").exists(),
        "Cursor local install should leave permissions to Cursor approval/run-mode behavior"
    );
    assert!(
        !home.path().join(".cursor/mcp.json").exists(),
        "local install must not write the legacy global Cursor MCP config"
    );
    assert!(
        !home.path().join(".tracedecay/config.toml").exists(),
        "local install must not create or mutate user-level install tracking"
    );
}

#[tokio::test]
async fn test_local_install_cursor_defers_branch_tracking_without_daemon() {
    let _env_lock = AGENT_ENV_LOCK.lock().await;
    let home = TempDir::new().unwrap();
    let home_root = home
        .path()
        .canonicalize()
        .unwrap_or_else(|_| home.path().to_path_buf());
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, home_root.join(".tracedecay"));
    let project = TempDir::new().unwrap();
    let project_root = project
        .path()
        .canonicalize()
        .unwrap_or_else(|_| project.path().to_path_buf());
    let git_init = Command::new("git")
        .arg("init")
        .arg("-b")
        .arg("main")
        .current_dir(&project_root)
        .output()
        .expect("git init should run");
    assert!(
        git_init.status.success(),
        "git init should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&git_init.stdout),
        String::from_utf8_lossy(&git_init.stderr)
    );
    std::fs::create_dir_all(project_root.join("src")).unwrap();
    std::fs::write(project_root.join("src/lib.rs"), "pub fn hello() {}\n").unwrap();
    let git_add = Command::new("git")
        .arg("add")
        .arg("src/lib.rs")
        .current_dir(&project_root)
        .output()
        .expect("git add should run");
    assert!(
        git_add.status.success(),
        "git add should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&git_add.stdout),
        String::from_utf8_lossy(&git_add.stderr)
    );
    let git_commit = Command::new("git")
        .arg("-c")
        .arg("user.name=TraceDecay Test")
        .arg("-c")
        .arg("user.email=tracedecay@example.invalid")
        .arg("commit")
        .arg("-m")
        .arg("initial")
        .current_dir(&project_root)
        .output()
        .expect("git commit should run");
    assert!(
        git_commit.status.success(),
        "git commit should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&git_commit.stdout),
        String::from_utf8_lossy(&git_commit.stderr)
    );
    let init = tracedecay_command(&project_root, &home_root)
        .arg("init")
        .output()
        .expect("TraceDecay init should run");
    assert!(
        init.status.success(),
        "TraceDecay init should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
    let checkout = Command::new("git")
        .arg("checkout")
        .arg("-b")
        .arg("feature/install")
        .current_dir(&project_root)
        .output()
        .expect("git checkout should run");
    assert!(
        checkout.status.success(),
        "git checkout should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&checkout.stdout),
        String::from_utf8_lossy(&checkout.stderr)
    );

    let output = assert_local_install_success("cursor", &project_root, &home_root);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "deferred Cursor branch tracking for 'feature/install' because the TraceDecay daemon request was unavailable"
        ),
        "Cursor install should report deferred daemon-owned branch tracking\nstderr:\n{stderr}"
    );

    let data_dir = resolve_layout_for_current_profile(&project_root)
        .unwrap_or_else(|err| panic!("failed to resolve project store layout: {err}"))
        .data_root;
    let meta = branch_meta::load_branch_meta(&data_dir)
        .expect("TraceDecay init should bootstrap branch tracking metadata");
    assert!(meta.is_tracked("main"));
    assert!(
        !meta.is_tracked("feature/install"),
        "Cursor install must not bypass the daemon to write branch metadata"
    );
}

#[test]
fn test_local_install_cursor_preserves_existing_permissions_file() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let cursor_dir = project.path().join(".cursor");
    std::fs::create_dir_all(&cursor_dir).unwrap();
    std::fs::write(
        cursor_dir.join("permissions.json"),
        r#"{
  "mcpAllowlist": [
    "other:custom_tool",
    "tracedecay:tracedecay_not_a_real_tool",
    "tracedecay:tracedecay_str_replace"
  ]
}
"#,
    )
    .unwrap();

    assert_local_install_success("cursor", project.path(), home.path());

    let permissions = std::fs::read_to_string(cursor_dir.join("permissions.json")).unwrap();
    assert!(permissions.contains("other:custom_tool"));
    assert!(permissions.contains("tracedecay:tracedecay_not_a_real_tool"));
    assert!(permissions.contains("tracedecay:tracedecay_str_replace"));
}

#[test]
fn test_cursor_healthcheck_ignores_foreign_project_cursor_files() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);
    let project = TempDir::new().unwrap();
    CursorIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let cursor_dir = project.path().join(".cursor");
    std::fs::create_dir_all(cursor_dir.join("rules")).unwrap();
    std::fs::write(
        cursor_dir.join("mcp.json"),
        r#"{"mcpServers":{"other":{"command":"other-bin"}}}"#,
    )
    .unwrap();
    std::fs::write(
        cursor_dir.join("hooks.json"),
        r#"{"version":1,"hooks":{"afterFileEdit":[{"command":"other-hook","timeout":30}]}}"#,
    )
    .unwrap();
    std::fs::write(
        cursor_dir.join("rules/tracedecay.mdc"),
        "---\nalwaysApply: false\n---\nforeign rule\n",
    )
    .unwrap();

    let mut dc = DoctorCounters::new();
    CursorIntegration.healthcheck(
        &mut dc,
        &HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: project.path().to_path_buf(),
        },
    );

    assert_eq!(dc.warnings, 0, "foreign Cursor files should not warn");
    assert_eq!(
        dc.issues, 0,
        "foreign Cursor files should not fail healthcheck"
    );
}

#[test]
fn test_local_install_cursor_removes_legacy_project_mcp_hooks_and_rule() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let cursor_dir = project.path().join(".cursor");
    std::fs::create_dir_all(cursor_dir.join("rules")).unwrap();
    std::fs::write(
        cursor_dir.join("mcp.json"),
        r#"{"mcpServers":{"tracedecay":{"type":"stdio","command":"/old/tracedecay","args":["serve","--path","."]}}}"#,
    )
    .unwrap();
    std::fs::write(
        cursor_dir.join("hooks.json"),
        r#"{"version":1,"hooks":{"afterFileEdit":[{"command":"/old/tracedecay hook-cursor-after-file-edit","timeout":30}]}}"#,
    )
    .unwrap();
    std::fs::write(
        cursor_dir.join("rules/tracedecay.mdc"),
        "---\ndescription: Prefer tracedecay MCP tools for codebase exploration\nalwaysApply: true\n---\n\n# Prefer tracedecay MCP tools\n",
    )
    .unwrap();

    // Installed twice on purpose: the first run removes the legacy files, and
    // the second must still succeed against the already-cleaned tree. Both
    // calls spawn the real CLI, so this is not a duplicated assertion.
    assert_local_install_success("cursor", project.path(), home.path());
    assert_local_install_success("cursor", project.path(), home.path());

    assert!(
        !cursor_dir.join("mcp.json").exists(),
        "local install should remove project-local MCP config"
    );
    assert!(
        !cursor_dir.join("hooks.json").exists(),
        "local install should remove project-local hooks"
    );
    assert!(
        !cursor_dir.join("rules/tracedecay.mdc").exists(),
        "local install should remove project-local rule"
    );
}

/// Global `tracedecay install --agent cursor` runs with the project as cwd and
/// must sweep project-local tracedecay artifacts there (old installs
/// predate the plugin), while preserving user-authored entries alongside them.
#[test]
fn test_global_install_cursor_sweeps_legacy_project_artifacts_at_cwd() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let cursor_dir = project.path().join(".cursor");
    std::fs::create_dir_all(cursor_dir.join("rules")).unwrap();
    std::fs::write(
        cursor_dir.join("mcp.json"),
        r#"{"mcpServers":{"tracedecay":{"type":"stdio","command":"/old/tracedecay","args":["serve","--path","."]},"other":{"url":"https://example.com/mcp"}}}"#,
    )
    .unwrap();
    std::fs::write(
        cursor_dir.join("rules/tracedecay.mdc"),
        "# Prefer tracedecay MCP tools\n",
    )
    .unwrap();

    let output = tracedecay_command(project.path(), home.path())
        .arg("install")
        .arg("--agent")
        .arg("cursor")
        .output()
        .expect("run global cursor install");
    assert!(
        output.status.success(),
        "global cursor install should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        home.path()
            .join(".cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json")
            .exists(),
        "the user-level plugin should be installed"
    );
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cursor_dir.join("mcp.json")).unwrap())
            .unwrap();
    assert!(
        mcp["mcpServers"].get("tracedecay").is_none(),
        "project-local MCP entry should be swept"
    );
    assert!(
        mcp["mcpServers"].get("other").is_some(),
        "user-authored project MCP servers must be preserved"
    );
    assert!(
        !cursor_dir.join("rules/tracedecay.mdc").exists(),
        "project-local rule should be swept"
    );
}

/// The project-local sweep must never modify files *through* a symlinked `.cursor`
/// that escapes the project. A symlinked `.cursor` with no TraceDecay
/// artifacts is left alone (the plugin owns all surfaces, so there is nothing
/// to write project-locally), but once legacy artifacts are detected behind
/// the symlink the install refuses rather than reaching outside the project.
#[cfg(unix)]
#[test]
fn test_local_install_cursor_rejects_symlinked_cursor_dir_with_legacy_artifacts() {
    use std::os::unix::fs::symlink;

    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let legacy_mcp = r#"{"mcpServers":{"tracedecay":{"type":"stdio","command":"/old/tracedecay","args":["serve","--path","."]}}}"#;
    std::fs::write(outside.path().join("mcp.json"), legacy_mcp).unwrap();
    symlink(outside.path(), project.path().join(".cursor")).unwrap();

    let output = run_local_install("cursor", project.path(), home.path());
    assert!(
        !output.status.success(),
        "local Cursor install should reject sweeping through a symlinked .cursor directory"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("symlink"),
        "error should explain the symlink refusal, got:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("mcp.json")).unwrap(),
        legacy_mcp,
        "files behind the symlink must be untouched"
    );
}

/// A symlinked `.cursor` containing only user config is harmless now that
/// the plugin owns MCP/hooks/rules and local install writes nothing
/// project-local — the install succeeds and the linked tree is untouched.
#[cfg(unix)]
#[test]
fn test_local_install_cursor_allows_legacy_free_symlinked_cursor_dir() {
    use std::os::unix::fs::symlink;

    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let user_mcp = r#"{"mcpServers":{"other":{"url":"https://example.com/mcp"}}}"#;
    std::fs::write(outside.path().join("mcp.json"), user_mcp).unwrap();
    symlink(outside.path(), project.path().join(".cursor")).unwrap();

    assert_local_install_success("cursor", project.path(), home.path());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("mcp.json")).unwrap(),
        user_mcp,
        "user config behind the symlink must be untouched"
    );
}

#[test]
fn test_cursor_install_creates_plugin() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    CursorIntegration.install(&ctx).unwrap();

    assert_cursor_plugin_bundle(
        &cursor_plugin_install_dir(home),
        &ctx.tracedecay_bin,
        env!("CARGO_PKG_VERSION"),
    );
    assert!(
        !home.join(".cursor/mcp.json").exists(),
        "Cursor plugin install should not write legacy ~/.cursor/mcp.json"
    );
}

#[tokio::test]
async fn test_cursor_install_exports_active_managed_skills() {
    let _env_lock = AGENT_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let profile_root = home.join(".tracedecay");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
    let mut draft = managed_skill_draft("repo-hygiene", "Repo Hygiene");
    draft.targets = vec![SkillInstallTarget::Cursor];
    create_managed_skill_draft(&profile_root, draft)
        .await
        .unwrap();
    approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();

    let ctx = make_install_ctx(home);
    CursorIntegration.install(&ctx).unwrap();

    let skill_path =
        cursor_plugin_install_dir(home).join("skills/agent-managed/repo-hygiene/SKILL.md");
    let skill = std::fs::read_to_string(skill_path).unwrap();
    assert!(skill.contains("name: repo-hygiene"));
    assert!(skill.contains("description:"));
    assert!(!skill.contains("id: repo-hygiene"));
    assert!(skill.contains("Use Repo Hygiene for repeated workflows."));
}

#[tokio::test]
async fn test_prompt_integrations_export_active_managed_skill_indexes() {
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
    // Pin the Kimi Code CLI home so a developer-set KIMI_CODE_HOME can never
    // redirect any kimi path resolution in this test into the real home.
    let _kimi_code_home = EnvVarGuard::set(
        tracedecay::agents::kimi::KIMI_CODE_HOME_ENV,
        home.join(".kimi-code"),
    );
    ClaudeIntegration.install(&ctx).unwrap();
    OpenCodeIntegration.install(&ctx).unwrap();
    CopilotIntegration.install(&ctx).unwrap();
    VibeIntegration.install(&ctx).unwrap();

    // Kimi's prompt-index surface is project-local (its global install is the
    // Kimi Code CLI plugin, which owns skills natively): exercise the same
    // managed-skill index export through install_local.
    let kimi_project = TempDir::new().unwrap();
    KimiIntegration
        .install_local(&ctx, kimi_project.path())
        .unwrap();

    for prompt_path in [
        home.join(".claude/CLAUDE.md"),
        kimi_project.path().join("AGENTS.md"),
        home.join(".config/opencode/AGENTS.md"),
        vscode_data_dir(home).join("User/prompts/copilot-instructions.md"),
        copilot_cli_dir(home).join("copilot-instructions.md"),
        home.join(".vibe/prompts/cli.md"),
    ] {
        let prompt = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(
            prompt.contains("TRACEDECAY MANAGED SKILLS START"),
            "missing managed skill block in {}",
            prompt_path.display()
        );
        assert!(prompt.contains("`repo-hygiene`"));
        assert!(prompt.contains("tracedecay_skill_view"));
    }

    ClaudeIntegration.uninstall(&ctx).unwrap();
    OpenCodeIntegration.uninstall(&ctx).unwrap();
    CopilotIntegration.uninstall(&ctx).unwrap();
    VibeIntegration.uninstall(&ctx).unwrap();

    // Kimi's project AGENTS.md is intentionally absent here: the project-local
    // index has no global uninstall surface. Removal of pre-plugin `~/.kimi`
    // prompt indexes is covered by the migration-shim uninstall test in
    // agent_install_test.rs.
    for prompt_path in [
        home.join(".claude/CLAUDE.md"),
        home.join(".config/opencode/AGENTS.md"),
        vscode_data_dir(home).join("User/prompts/copilot-instructions.md"),
        copilot_cli_dir(home).join("copilot-instructions.md"),
        home.join(".vibe/prompts/cli.md"),
    ] {
        if !prompt_path.exists() {
            continue;
        }
        let prompt = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(
            !prompt.contains("TRACEDECAY MANAGED SKILLS START")
                && !prompt.contains("TRACEDECAY MANAGED SKILLS END")
                && !prompt.contains("`repo-hygiene`"),
            "managed skill block should be removed from {}",
            prompt_path.display()
        );
    }
}

#[test]
fn test_cursor_install_preserves_existing_legacy_mcp_config() {
    let dir = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(dir.path());
    let path = dir.path().join(".cursor/mcp.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = r#"{
  "mcpServers": { "other": { "command": "other-bin" } }
}
"#;
    std::fs::write(&path, original).unwrap();

    CursorIntegration
        .install(&make_install_ctx(dir.path()))
        .unwrap();

    assert_cursor_plugin_bundle(
        &cursor_plugin_install_dir(dir.path()),
        &make_install_ctx(dir.path()).tracedecay_bin,
        env!("CARGO_PKG_VERSION"),
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        original,
        "plugin install should not rewrite legacy Cursor MCP config"
    );
    assert!(
        !dir.path().join(".cursor/mcp.json.bak").exists(),
        "plugin install should not create backups for untouched legacy MCP config"
    );
}

#[test]
fn test_cursor_uninstall_backs_up_config_with_other_content() {
    // Regression for issue #63: uninstall paths must also back up the file
    // before rewriting, so a botched rewrite is recoverable.
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    let path = home.join(".cursor/mcp.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = r#"{
  "mcpServers": {
    "tracedecay": { "command": "/usr/local/bin/tracedecay", "args": ["serve"] },
    "other": { "command": "other-bin" }
  }
}
"#;
    std::fs::write(&path, original).unwrap();

    CursorIntegration.uninstall(&ctx).unwrap();

    let backup = home.join(".cursor/mcp.json.bak");
    assert!(
        backup.exists(),
        "uninstall must back up the existing config before rewriting it"
    );
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        original,
        "backup must contain the exact pre-uninstall bytes"
    );
    let new = std::fs::read_to_string(&path).unwrap();
    assert!(
        new.contains("other-bin") && !new.contains("tracedecay"),
        "uninstall must drop tracedecay but keep other servers; got:\n{new}"
    );
}

#[test]
fn test_cursor_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);

    CursorIntegration.install(&ctx).unwrap();
    let plugin_dir = cursor_plugin_install_dir(home);
    assert!(plugin_dir.exists());
    let legacy_digest = plugin_dir.join("rules/tracedecay-memory-digest.mdc");
    std::fs::create_dir_all(legacy_digest.parent().unwrap()).unwrap();
    std::fs::write(&legacy_digest, "legacy digest").unwrap();
    seed_memory_digest_target(
        &home.join(".tracedecay"),
        tracedecay::automation::skill_targets::SkillInstallTarget::Cursor,
        &plugin_dir,
    );

    CursorIntegration.uninstall(&ctx).unwrap();

    assert!(
        !plugin_dir.exists(),
        "Cursor uninstall should remove the local plugin install"
    );
    std::fs::create_dir_all(&plugin_dir).unwrap();
    tracedecay::automation::memory_digest::export_memory_digest_to_recorded_targets(
        &home.join(".tracedecay"),
    )
    .unwrap();
    assert!(
        !legacy_digest.exists(),
        "Cursor uninstall must unrecord legacy memory digest targets"
    );
}

#[test]
fn test_healthcheck_cursor_clean_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    CursorIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    CursorIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "clean Cursor install should have no issues");
}

#[test]
fn test_healthcheck_cursor_local_install_checks_project_config() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    assert_local_install_success("cursor", project.path(), home.path());

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    CursorIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.issues, 0,
        "local Cursor healthcheck should pass without global ~/.cursor config"
    );
}

#[test]
fn test_cursor_healthcheck_warns_on_literal_workspace_folder_transcript_path() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    CursorIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();
    let daemon_status = serde_json::json!({
        "cursor_session_ingest": {
            "tracked_transcripts": 1,
            "pending_transcripts": 0,
            "pending_bytes": 0,
            "max_transcript_pending_bytes": 0,
        },
        "cursor_session_placeholder_paths": [
            "${workspaceFolder}/.cursor/sessions/cursor-placeholder-session.jsonl"
        ],
    });

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    CursorIntegration.healthcheck_with_daemon_status(&mut dc, &hctx, Some(&daemon_status));
    assert_eq!(dc.issues, 0);
    assert_eq!(dc.warnings, 1);
}

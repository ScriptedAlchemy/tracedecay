use super::super::safe_write_json_file;
use super::*;
use serde_json::json;

#[test]
fn missing_manifest_with_stale_registration_is_repairable() {
    use crate::agents::AgentIntegration;
    use crate::agents::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};

    let home = tempfile::TempDir::new().unwrap();
    let project = tempfile::TempDir::new().unwrap();
    let marketplace = known_marketplaces_path(home.path());
    std::fs::create_dir_all(marketplace.parent().unwrap()).unwrap();
    safe_write_json_file(
        &marketplace,
        &json!({
            "tracedecay": {
                "source": { "source": "directory", "path": "/stale" }
            }
        }),
        None,
    )
    .unwrap();
    let state = ClaudeIntegration.host_component_registration(
        HostBundleComponentV1::Core,
        &HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: project.path().to_path_buf(),
        },
    );
    assert_eq!(state, HostBundleRegistrationStateV1::Repairable);
}

#[test]
fn missing_manifest_with_partial_settings_residue_is_repairable() {
    use crate::agents::AgentIntegration;
    use crate::agents::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};

    let home = tempfile::TempDir::new().unwrap();
    let project = tempfile::TempDir::new().unwrap();
    let settings = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    safe_write_json_file(
        &settings,
        &json!({
            "enabledPlugins": { "tracedecay@tracedecay": false },
            "permissions": { "allow": ["mcp__tracedecay__search"] }
        }),
        None,
    )
    .unwrap();
    let state = ClaudeIntegration.host_component_registration(
        HostBundleComponentV1::Core,
        &HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: project.path().to_path_buf(),
        },
    );
    assert_eq!(state, HostBundleRegistrationStateV1::Repairable);
}

#[test]
fn project_only_legacy_residue_does_not_claim_plugin_registration() {
    use crate::agents::AgentIntegration;
    use crate::agents::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};

    let home = tempfile::TempDir::new().unwrap();
    let project = tempfile::TempDir::new().unwrap();
    safe_write_json_file(
        &project.path().join(".mcp.json"),
        &json!({ "mcpServers": { "tracedecay": { "command": "old" } } }),
        None,
    )
    .unwrap();
    let state = ClaudeIntegration.host_component_registration(
        HostBundleComponentV1::Core,
        &HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: project.path().to_path_buf(),
        },
    );
    assert_eq!(state, HostBundleRegistrationStateV1::Missing);
}

fn plugin_subdir_names(rel: &str) -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugin")
        .join(rel);
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .expect("plugin source dir should be readable")
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Every file under a skills root, relative to it, forward-slashed.
fn plugin_skill_tree_files(root: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir)
            .expect("skills dir readable")
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else if path.is_file() {
                out.push(
                    path.strip_prefix(base)
                        .expect("under base")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    files
}

fn install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: vec!["mcp__tracedecay__search".to_string()],
        project_root: None,
        dashboard: true,
    }
}

/// The composed Claude deploy set (sourced from the shared `plugin/` tree
/// via `claude_files`) must cover every shared model-invocable skill, the
/// 13 canonical `tracedecay-*` dispatchers, all 8 subagents, all 13 slash
/// commands, and Claude's manifest/marketplace/mcp/hooks/README. The single
/// shared tree removes the old cross-bundle parity checks; this guards that
/// nothing on disk is left unwired for Claude.
#[test]
fn claude_embedded_file_list_covers_the_whole_source_bundle() {
    let deploy: std::collections::BTreeSet<String> = claude_embedded_plugin_files()
        .into_iter()
        .map(|(relative, _)| relative.to_string())
        .collect();

    let skills = plugin_subdir_names("skills");
    assert_eq!(skills.len(), 15, "expected 15 shared skill dirs");
    // Every file under plugin/skills/ (SKILL.md *and* any support files) is
    // deployed — the recursive embed leaves nothing on disk unwired.
    let skills_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin/skills");
    for relative in plugin_skill_tree_files(&skills_root) {
        let expected = format!("skills/{relative}");
        assert!(
            deploy.contains(&expected),
            "Claude deploy set is missing skill file {expected}"
        );
    }

    for expected in [
        ".claude-plugin/plugin.json",
        ".claude-plugin/marketplace.json",
        ".mcp.json",
        "hooks/hooks.json",
        "README.md",
    ] {
        assert!(
            deploy.contains(expected),
            "Claude deploy set is missing {expected}"
        );
    }

    // Every agent on disk under plugin/agents is deployed — dir-walk rather
    // than hardcode, so a future agent added to the shared source tree but
    // not wired into Claude's deploy set is caught here.
    let agents_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin/agents");
    for entry in std::fs::read_dir(&agents_root).expect("plugin/agents readable") {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        let expected = format!("agents/{name}");
        assert!(
            deploy.contains(&expected),
            "Claude deploy set is missing agent {expected}"
        );
    }

    // Every command in plugin/commands is deployed.
    let commands_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin/commands");
    for entry in std::fs::read_dir(&commands_root).expect("plugin/commands readable") {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        let expected = format!("commands/{name}");
        assert!(
            deploy.contains(&expected),
            "Claude deploy set is missing command {expected}"
        );
    }
}

/// Deploy stamps the crate version into plugin.json, substitutes the
/// binary path into hooks.json and .mcp.json, and leaves no placeholder.
#[test]
fn deploy_stamps_version_and_binary_path() {
    let home = tempfile::tempdir().unwrap();
    let deploy_dir = deploy_plugin_bundle(home.path(), "/abs/bin/tracedecay").unwrap();

    let plugin: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(deploy_dir.join(".claude-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(plugin["version"].as_str().unwrap(), crate::PRODUCT_VERSION);

    let hooks = std::fs::read_to_string(deploy_dir.join("hooks/hooks.json")).unwrap();
    assert!(
        !hooks.contains(TRACEDECAY_BIN_PLACEHOLDER),
        "placeholder must be substituted"
    );
    assert!(hooks.contains("/abs/bin/tracedecay"));

    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(deploy_dir.join(".mcp.json")).unwrap())
            .unwrap();
    assert_eq!(
        mcp["mcpServers"]["graph"]["command"].as_str().unwrap(),
        "/abs/bin/tracedecay"
    );
}

/// A binary path carrying a JSON-special char must be escaped via serde so
/// the deployed hooks.json stays valid JSON (regression: a raw
/// `str::replace` into the JSON text produced invalid output).
#[test]
fn deploy_escapes_special_chars_in_binary_path() {
    let home = tempfile::tempdir().unwrap();
    let weird_bin = "/opt/td \"quote\"/tracedecay";
    let deploy_dir = deploy_plugin_bundle(home.path(), weird_bin).unwrap();

    let hooks_raw = std::fs::read_to_string(deploy_dir.join("hooks/hooks.json")).unwrap();
    // Must parse — a raw replace would have produced invalid JSON here.
    let hooks: serde_json::Value = serde_json::from_str(&hooks_raw)
        .expect("hooks.json must stay valid JSON after binary-path substitution");
    assert!(
        !hooks_raw.contains(TRACEDECAY_BIN_PLACEHOLDER),
        "placeholder must be fully substituted"
    );
    let command = hooks["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert_eq!(command, weird_bin, "command must be the exact binary path");
}

/// Redeploy must be a CLEAN REPLACE of the owned marketplace dir: a stale
/// file the current bundle no longer ships (e.g. a retired skill dir) is
/// gone after a redeploy, while the fresh bundle is present.
#[test]
fn deploy_is_a_clean_replace_dropping_stale_files() {
    let home = tempfile::tempdir().unwrap();
    let deploy_dir = deploy_plugin_bundle(home.path(), "/bin/tracedecay").unwrap();
    // A stale skill dir the current bundle does not ship.
    let stale = deploy_dir.join("skills/totally-retired-skill");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("SKILL.md"), "stale skill").unwrap();

    // Redeploy (the install/update path).
    deploy_plugin_bundle(home.path(), "/bin/tracedecay").unwrap();

    assert!(
        !stale.exists(),
        "a stale skill dir must be gone after a clean-replace redeploy"
    );
    assert!(
        deploy_dir.join(".claude-plugin/plugin.json").exists(),
        "the fresh bundle must be present after redeploy"
    );
}

/// The clean replace must refuse to delete a marketplace dir tracedecay
/// does not own (no tracedecay plugin/marketplace manifest), so an
/// unrelated dir squatting on the path is never nuked.
#[test]
fn deploy_refuses_to_replace_non_tracedecay_dir() {
    let home = tempfile::tempdir().unwrap();
    let deploy_dir = plugin_deploy_dir(home.path());
    std::fs::create_dir_all(deploy_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        deploy_dir.join(".claude-plugin/plugin.json"),
        r#"{"name":"someone-elses-plugin"}"#,
    )
    .unwrap();
    std::fs::write(deploy_dir.join("user-file.txt"), "keep me").unwrap();

    let err = deploy_plugin_bundle(home.path(), "/bin/tracedecay")
        .expect_err("must refuse a non-tracedecay dir");
    assert!(
        err.to_string().contains("non-tracedecay"),
        "unexpected error: {err}"
    );
    assert!(
        deploy_dir.join("user-file.txt").exists(),
        "an unowned dir must be left untouched"
    );
}

/// Staging must leave host-native marketplace and settings files untouched.
#[test]
fn install_stages_source_without_rewriting_host_state() {
    let home = tempfile::tempdir().unwrap();
    let ctx = install_ctx(home.path());
    let settings_path = home.path().join(".claude/settings.json");
    let known_path = home.path().join(".claude/plugins/known_marketplaces.json");
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(&settings_path, br#"{"enabledPlugins":{"other":true}}"#).unwrap();
    std::fs::write(&known_path, br#"{"other":{"source":{"source":"github"}}}"#).unwrap();
    let settings_before = std::fs::read(&settings_path).unwrap();
    let known_before = std::fs::read(&known_path).unwrap();

    let error = ClaudeIntegration.install(&ctx).unwrap_err().to_string();
    assert!(error.contains("Claude Code owns marketplace registration"));
    assert!(plugin_marketplace_manifest_path(home.path()).is_file());
    assert_eq!(std::fs::read(settings_path).unwrap(), settings_before);
    assert_eq!(std::fs::read(known_path).unwrap(), known_before);
}

/// The managed-block range must extend across only its own owned
/// sub-heading, not a user's own `## …tracedecay…` heading placed after
/// the block — otherwise uninstall would swallow the user's section.
#[test]
fn uninstall_preserves_user_tracedecay_heading_after_block() {
    let home = tempfile::tempdir().unwrap();
    let claude_md = home.path().join("CLAUDE.md");
    install_claude_md_rules(&claude_md).unwrap();

    // Append a user-authored heading whose text contains "tracedecay".
    let user_section = "\n## Using tracedecay in CI\n\nRun `tracedecay serve` in the pipeline.\n";
    let mut contents = std::fs::read_to_string(&claude_md).unwrap();
    contents.push_str(user_section);
    std::fs::write(&claude_md, &contents).unwrap();

    uninstall_claude_md_rules(&claude_md).unwrap();

    let after = std::fs::read_to_string(&claude_md).unwrap();
    assert!(
        after.contains("## Using tracedecay in CI"),
        "the user's own tracedecay heading must survive uninstall"
    );
    assert!(
        after.contains("Run `tracedecay serve` in the pipeline."),
        "the user's own section body must survive uninstall"
    );
    assert!(
        !after.contains(CLAUDE_MD_MARKER),
        "the managed block itself must be removed"
    );
}

#[test]
fn uninstall_after_native_removal_cleans_source_without_rewriting_host_state() {
    let home = tempfile::tempdir().unwrap();
    let ctx = install_ctx(home.path());
    deploy_plugin_bundle(home.path(), &ctx.tracedecay_bin).unwrap();
    let settings_path = home.path().join(".claude/settings.json");
    let marketplace_path = known_marketplaces_path(home.path());
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(&settings_path, br#"{"enabledPlugins":{"other":true}}"#).unwrap();
    std::fs::write(
        &marketplace_path,
        br#"{"other":{"source":{"source":"github"}}}"#,
    )
    .unwrap();
    let settings_before = std::fs::read(&settings_path).unwrap();
    let marketplace_before = std::fs::read(&marketplace_path).unwrap();
    assert!(plugin_marketplace_manifest_path(home.path()).exists());

    ClaudeIntegration.uninstall(&ctx).unwrap();
    assert!(
        !plugin_deploy_dir(home.path()).exists(),
        "deploy dir removed"
    );
    assert_eq!(std::fs::read(settings_path).unwrap(), settings_before);
    assert_eq!(std::fs::read(marketplace_path).unwrap(), marketplace_before);
}

#[cfg(target_os = "linux")]
#[test]
fn install_claude_md_rules_surfaces_append_failures() {
    let err = install_claude_md_rules(Path::new("/dev/full")).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("failed to append tracedecay rules to /dev/full"),
        "unexpected error message: {msg}"
    );
}

/// Every managed subagent definition the plugin ships must have valid
/// frontmatter and reference tracedecay.
#[test]
fn managed_subagent_definitions_have_valid_frontmatter() {
    let files = claude_embedded_plugin_files();
    for file_name in [
        "code-explorer.md",
        "code-health-auditor.md",
        "session-historian.md",
    ] {
        let contents = files
            .iter()
            .find_map(|&(relative, body)| {
                (relative == format!("agents/{file_name}")).then_some(body)
            })
            .expect("plugin must ship each managed subagent");
        let stem = file_name.trim_end_matches(".md");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(
            lines.first().copied(),
            Some("---"),
            "{file_name} must open YAML frontmatter"
        );
        let expected_name = format!("name: {stem}");
        assert!(
            lines.contains(&expected_name.as_str()),
            "{file_name} frontmatter name must match its filename"
        );
        assert!(
            lines.iter().any(|line| line.starts_with("description: ")),
            "{file_name} must carry a description for delegation"
        );
        assert!(
            contents.contains("tracedecay"),
            "{file_name} must reference tracedecay so it is recognized as managed"
        );
    }
}

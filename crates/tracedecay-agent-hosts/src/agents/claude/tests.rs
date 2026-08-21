use super::super::{load_json_file_strict, safe_write_json_file};
use super::*;
use serde_json::json;
use std::path::PathBuf;

/// Shared `plugin/` source tree at the repo root, relative to this crate.
fn plugin_source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugin")
}

fn copy_rendered_bundle_to_native_cache(home: &Path, tracedecay_bin: &str) {
    let source = plugin_deploy_dir(home);
    let cache = claude_current_cached_plugin_root(home);
    for (relative, _) in rendered_plugin_files(tracedecay_bin).unwrap() {
        let target = cache.join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(source.join(relative), target).unwrap();
    }
}

fn write_native_activation(home: &Path, tracedecay_bin: &str) {
    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    safe_write_json_file(
        &settings,
        &json!({"enabledPlugins": {"tracedecay@tracedecay": true}}),
        None,
    )
    .unwrap();
    safe_write_json_file(
        &known_marketplaces_path(home),
        &json!({
            "tracedecay": {
                "source": {
                    "source": "directory",
                    "path": plugin_deploy_dir(home),
                },
                "installLocation": plugin_deploy_dir(home),
            }
        }),
        None,
    )
    .unwrap();
    copy_rendered_bundle_to_native_cache(home, tracedecay_bin);
}

#[test]
fn native_activation_requires_exact_catalog_mount_and_versioned_cache() {
    let home = tempfile::tempdir().unwrap();
    deploy_plugin_bundle(home.path(), "/bin/tracedecay").unwrap();
    write_native_activation(home.path(), "/bin/tracedecay");
    assert!(claude_plugin_is_natively_active(home.path(), Some("/bin/tracedecay")).unwrap());

    let marketplace = known_marketplaces_path(home.path());
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marketplace).unwrap()).unwrap();
    state["tracedecay"]["installLocation"] = json!("/different/marketplace");
    safe_write_json_file(&marketplace, &state, None).unwrap();
    assert!(!claude_plugin_is_natively_active(home.path(), Some("/bin/tracedecay")).unwrap());
}

#[test]
fn native_activation_rejects_current_version_manifest_in_unbound_cache_directory() {
    let home = tempfile::tempdir().unwrap();
    deploy_plugin_bundle(home.path(), "/bin/tracedecay").unwrap();
    write_native_activation(home.path(), "/bin/tracedecay");
    let exact = claude_current_cached_plugin_manifest_path(home.path());
    let unbound = exact
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("current/.claude-plugin/plugin.json");
    std::fs::create_dir_all(unbound.parent().unwrap()).unwrap();
    std::fs::rename(&exact, &unbound).unwrap();

    assert!(!claude_plugin_is_natively_active(home.path(), Some("/bin/tracedecay")).unwrap());
}

#[test]
fn native_cache_content_drift_and_binary_relocation_require_refresh() {
    let home = tempfile::tempdir().unwrap();
    let old_bin = "/old/bin/tracedecay";
    let new_bin = "/relocated/bin/tracedecay";
    deploy_plugin_bundle(home.path(), old_bin).unwrap();
    write_native_activation(home.path(), old_bin);
    let old_ctx = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: old_bin.to_string(),
        tool_permissions: Vec::new(),
        project_root: None,
        dashboard: true,
    };
    assert!(matches!(
        ClaudeIntegration
            .preflight_non_interactive_install(&old_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::Ready
    ));

    let retired_command =
        claude_current_cached_plugin_root(home.path()).join("commands/retired.md");
    std::fs::create_dir_all(retired_command.parent().unwrap()).unwrap();
    std::fs::write(&retired_command, "# stale auto-discovered command\n").unwrap();
    assert!(matches!(
        ClaudeIntegration
            .preflight_non_interactive_install(&old_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::DeferredUserAction(_)
    ));
    std::fs::remove_file(retired_command).unwrap();
    assert!(matches!(
        ClaudeIntegration
            .preflight_non_interactive_install(&old_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::Ready
    ));

    std::fs::write(
        claude_current_cached_plugin_root(home.path()).join(".mcp.json"),
        "{}\n",
    )
    .unwrap();
    assert!(matches!(
        ClaudeIntegration
            .preflight_non_interactive_install(&old_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::DeferredUserAction(_)
    ));
    copy_rendered_bundle_to_native_cache(home.path(), old_bin);
    assert!(matches!(
        ClaudeIntegration
            .preflight_non_interactive_install(&old_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::Ready
    ));

    deploy_plugin_bundle(home.path(), new_bin).unwrap();
    let relocated_ctx = InstallContext {
        tracedecay_bin: new_bin.to_string(),
        ..old_ctx
    };
    assert!(matches!(
        ClaudeIntegration
            .preflight_non_interactive_install(&relocated_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::DeferredUserAction(_)
    ));
    copy_rendered_bundle_to_native_cache(home.path(), new_bin);
    assert!(matches!(
        ClaudeIntegration
            .preflight_non_interactive_install(&relocated_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::Ready
    ));
}

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
    let root = plugin_source_root().join(rel);
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
    assert_eq!(skills.len(), 17, "expected 17 shared skill dirs");
    // Every file under plugin/skills/ (SKILL.md *and* any support files) is
    // deployed — the recursive embed leaves nothing on disk unwired.
    let skills_root = plugin_source_root().join("skills");
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
    let agents_root = plugin_source_root().join("agents");
    for entry in std::fs::read_dir(&agents_root).expect("plugin/agents readable") {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        let expected = format!("agents/{name}");
        assert!(
            deploy.contains(&expected),
            "Claude deploy set is missing agent {expected}"
        );
    }

    // Every command in plugin/commands is deployed.
    let commands_root = plugin_source_root().join("commands");
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

#[cfg(target_os = "linux")]
#[test]
fn install_claude_md_rules_surfaces_append_failures() {
    let err = install_claude_md_rules(Path::new("/dev/full")).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("failed to capture metadata for /dev/full"),
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

// ---------------------------------------------------------------------------
// Host-CLI-driven lifecycle
//
// Claude Code owns marketplace registration, cache, and enabled state, so
// TraceDecay drives `claude plugin ...` rather than writing those files. These
// tests stand a fake `claude` launcher in an isolated HOME, assert the exact
// argv TraceDecay issues, and assert that an absent binary refuses instead of
// falling back to config surgery.
// ---------------------------------------------------------------------------

/// Install a fake `claude` that appends each invocation's argv to `log` and
/// then performs `body` (so a test can have it "activate" the plugin the way
/// the real CLI would).
#[cfg(unix)]
fn fake_claude_cli(bin: &Path, log: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n{body}\n",
        log = shell_single_quote(&log.to_string_lossy()),
    );
    std::fs::write(bin, script).unwrap();
    let mut permissions = std::fs::metadata(bin).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(bin, permissions).unwrap();
}

#[cfg(unix)]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(unix)]
fn recorded_invocations(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[cfg(unix)]
#[test]
fn activation_drives_the_hosts_own_marketplace_and_install_commands() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let claude = bin_dir.path().join("claude");
    deploy_plugin_bundle(home.path(), "/bin/tracedecay").unwrap();
    fake_claude_cli(&claude, &log, "exit 0");

    claude_plugin_activate_with(&claude, home.path())
        .expect("a clean host CLI run is a completed activation");

    let deploy = plugin_deploy_dir(home.path());
    assert_eq!(
        recorded_invocations(&log),
        vec![
            format!("plugin marketplace add {}", deploy.display()),
            "plugin install tracedecay@tracedecay".to_string(),
        ],
        "activation must register the staged marketplace, then enable the plugin by \
         <plugin>@<marketplace>"
    );
}

#[cfg(unix)]
#[test]
fn removal_drives_the_hosts_own_uninstall_by_plugin_selection_name() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let claude = bin_dir.path().join("claude");
    fake_claude_cli(&claude, &log, "exit 0");

    claude_plugin_deactivate_with(&claude, home.path())
        .expect("a clean host CLI run is a completed removal");

    assert_eq!(
        recorded_invocations(&log),
        vec![
            "plugin uninstall tracedecay".to_string(),
            "plugin marketplace remove tracedecay".to_string(),
        ],
        "uninstall addresses the plugin by selection name; only the marketplace entry \
         is removed by marketplace name"
    );
}

#[cfg(unix)]
#[test]
fn a_failing_host_command_reports_the_hosts_own_diagnosis() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let claude = bin_dir.path().join("claude");
    fake_claude_cli(
        &claude,
        &log,
        "echo 'plugin tracedecay is not installed' >&2\nexit 4",
    );

    let error = claude_plugin_deactivate_with(&claude, home.path())
        .expect_err("a non-zero host CLI exit must fail the lifecycle");

    let TraceDecayError::Config { message } = error else {
        panic!("a failed host command must surface as a config error");
    };
    assert!(
        message.contains("plugin tracedecay is not installed") && message.contains("exit code 4"),
        "the host's own stderr and status must reach the operator: {message}"
    );
}

#[test]
fn a_missing_host_binary_refuses_instead_of_editing_host_owned_state() {
    let home = tempfile::tempdir().unwrap();
    deploy_plugin_bundle(home.path(), "/bin/tracedecay").unwrap();
    let before = std::fs::read(known_marketplaces_path(home.path())).ok();

    let error =
        crate::agents::host_cli::require_host_cli("claude-definitely-absent", CLAUDE_CLI_LIFECYCLE)
            .expect_err("an absent host binary is a hard requirement failure");

    let TraceDecayError::HostCliUnavailable { program, lifecycle } = error else {
        panic!("host CLI absence must surface as a typed requirement");
    };
    assert_eq!(program, "claude-definitely-absent");
    assert_eq!(lifecycle, CLAUDE_CLI_LIFECYCLE);
    assert_eq!(
        std::fs::read(known_marketplaces_path(home.path())).ok(),
        before,
        "a refused lifecycle must not have touched host-owned registration state"
    );
}

/// The single documented wildcard rule must satisfy the permission check on
/// its own, exactly like a full per-tool grant, while partial grants keep the
/// prompt warning truthful. An empty expected list (no registered tool
/// catalog) must not read as vacuously satisfied.
#[test]
fn plugin_permission_coverage_accepts_wildcard_or_full_per_tool_grants() {
    let wildcard = plugin_wildcard_perm();
    assert_eq!(wildcard, "mcp__plugin_tracedecay_graph__*");

    let per_tool = vec![
        format!("{PLUGIN_TOOL_PERM_PREFIX}tracedecay_search"),
        format!("{PLUGIN_TOOL_PERM_PREFIX}tracedecay_grep"),
    ];
    let all: Vec<&str> = per_tool.iter().map(String::as_str).collect();

    assert!(plugin_perms_covered(&[wildcard.as_str()], &per_tool));
    assert!(plugin_perms_covered(&all, &per_tool));
    assert!(
        !plugin_perms_covered(&all[..1], &per_tool),
        "one missing per-tool grant without the wildcard still prompts"
    );
    assert!(!plugin_perms_covered(&[], &per_tool));
    assert!(
        !plugin_perms_covered(&all, &[]),
        "an empty expected-tool list must not read as vacuously satisfied"
    );
    assert!(
        plugin_perms_covered(&[wildcard.as_str()], &[]),
        "the wildcard rule covers the namespace even without a registered catalog"
    );
}

#[test]
fn activation_adds_wildcard_permission_without_replacing_user_settings() {
    let home = tempfile::tempdir().unwrap();
    let tracedecay_bin = "/bin/tracedecay";
    deploy_plugin_bundle(home.path(), tracedecay_bin).unwrap();
    write_native_activation(home.path(), tracedecay_bin);

    let settings_path = home.path().join(".claude/settings.json");
    let existing = json!({
        "enabledPlugins": {
            "foreign@market": true,
            "tracedecay@tracedecay": true
        },
        "env": { "FOREIGN_SETTING": "preserved" },
        "permissions": {
            "allow": ["Read"],
            "deny": ["Bash(rm:*)"]
        }
    });
    safe_write_json_file(&settings_path, &existing, None).unwrap();
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: tracedecay_bin.to_string(),
        tool_permissions: Vec::new(),
        project_root: None,
        dashboard: true,
    };

    ClaudeIntegration
        .activate_deployed_host_registration(&ctx)
        .unwrap();
    ClaudeIntegration
        .activate_deployed_host_registration(&ctx)
        .unwrap();

    let updated = load_json_file_strict(&settings_path).unwrap();
    assert_eq!(updated["env"], existing["env"]);
    assert_eq!(
        updated["enabledPlugins"]["foreign@market"],
        existing["enabledPlugins"]["foreign@market"]
    );
    assert_eq!(
        updated["permissions"]["deny"],
        existing["permissions"]["deny"]
    );
    assert_eq!(
        updated["permissions"]["allow"],
        json!(["Read", "mcp__plugin_tracedecay_graph__*"]),
        "activation must add the one documented plugin wildcard exactly once"
    );
}

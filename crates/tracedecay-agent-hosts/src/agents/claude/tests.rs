use super::super::{load_json_file_strict, safe_write_json_file, safe_write_text_file};
use super::*;
use serde_json::json;

/// Writes the rendered marketplace source exactly where the component
/// catalog deploys it.
fn deploy_rendered_bundle(home: &Path, tracedecay_bin: &str) -> PathBuf {
    let deploy_dir = plugin_deploy_dir(home);
    for (relative, rendered) in rendered_plugin_files(tracedecay_bin).unwrap() {
        safe_write_text_file(&deploy_dir.join(relative), &rendered).unwrap();
    }
    deploy_dir
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
    )
    .unwrap();
    copy_rendered_bundle_to_native_cache(home, tracedecay_bin);
}

#[test]
fn native_activation_requires_exact_catalog_mount_and_versioned_cache() {
    let home = tempfile::tempdir().unwrap();
    deploy_rendered_bundle(home.path(), "/bin/tracedecay");
    write_native_activation(home.path(), "/bin/tracedecay");
    assert!(claude_plugin_is_natively_active(home.path(), Some("/bin/tracedecay")).unwrap());

    let marketplace = known_marketplaces_path(home.path());
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marketplace).unwrap()).unwrap();
    state["tracedecay"]["installLocation"] = json!("/different/marketplace");
    safe_write_json_file(&marketplace, &state).unwrap();
    assert!(!claude_plugin_is_natively_active(home.path(), Some("/bin/tracedecay")).unwrap());
}

#[test]
fn native_activation_rejects_current_version_manifest_in_unbound_cache_directory() {
    let home = tempfile::tempdir().unwrap();
    deploy_rendered_bundle(home.path(), "/bin/tracedecay");
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
    deploy_rendered_bundle(home.path(), old_bin);
    write_native_activation(home.path(), old_bin);
    assert!(claude_plugin_is_natively_active(home.path(), Some(old_bin)).unwrap());

    let retired_command =
        claude_current_cached_plugin_root(home.path()).join("commands/retired.md");
    std::fs::create_dir_all(retired_command.parent().unwrap()).unwrap();
    std::fs::write(&retired_command, "# stale auto-discovered command\n").unwrap();
    assert!(!claude_plugin_is_natively_active(home.path(), Some(old_bin)).unwrap());
    std::fs::remove_file(retired_command).unwrap();
    assert!(claude_plugin_is_natively_active(home.path(), Some(old_bin)).unwrap());

    std::fs::write(
        claude_current_cached_plugin_root(home.path()).join(".mcp.json"),
        "{}\n",
    )
    .unwrap();
    assert!(!claude_plugin_is_natively_active(home.path(), Some(old_bin)).unwrap());
    copy_rendered_bundle_to_native_cache(home.path(), old_bin);
    assert!(claude_plugin_is_natively_active(home.path(), Some(old_bin)).unwrap());

    deploy_rendered_bundle(home.path(), new_bin);
    assert!(!claude_plugin_is_natively_active(home.path(), Some(new_bin)).unwrap());
    copy_rendered_bundle_to_native_cache(home.path(), new_bin);
    assert!(claude_plugin_is_natively_active(home.path(), Some(new_bin)).unwrap());
}

#[test]
fn missing_manifest_with_stale_registration_is_repairable() {
    use crate::agents::AgentIntegration;
    use crate::agents::host_bundle::{HostBundleRegistrationStateV1, HostComponentV1};

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
    )
    .unwrap();
    let state = ClaudeIntegration.host_component_registration(
        HostComponentV1::Core,
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
    use crate::agents::host_bundle::{HostBundleRegistrationStateV1, HostComponentV1};

    let home = tempfile::TempDir::new().unwrap();
    let project = tempfile::TempDir::new().unwrap();
    safe_write_json_file(
        &project.path().join(".mcp.json"),
        &json!({ "mcpServers": { "tracedecay": { "command": "old" } } }),
    )
    .unwrap();
    let state = ClaudeIntegration.host_component_registration(
        HostComponentV1::Core,
        &HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: project.path().to_path_buf(),
        },
    );
    assert_eq!(state, HostBundleRegistrationStateV1::Missing);
}

/// Deploy stamps the crate version into plugin.json, substitutes the
/// binary path into hooks.json and .mcp.json, and leaves no placeholder.
#[test]
fn deploy_stamps_version_and_binary_path() {
    let home = tempfile::tempdir().unwrap();
    let deploy_dir = deploy_rendered_bundle(home.path(), "/abs/bin/tracedecay");

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
    let deploy_dir = deploy_rendered_bundle(home.path(), weird_bin);

    let hooks_raw = std::fs::read_to_string(deploy_dir.join("hooks/hooks.json")).unwrap();
    // Must parse, a raw replace would have produced invalid JSON here.
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

/// The managed-block range must extend across only its own owned
/// sub-heading, not a user's own `## …tracedecay…` heading placed after
/// the block, otherwise uninstall would swallow the user's section.
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
        !after.contains(CLAUDE_MD_SENTINELS.start),
        "the managed block itself must be removed"
    );
}

#[test]
fn claude_md_ownership_is_the_sentinel_not_prose() {
    let prose = "Use tracedecay MCP tools and never spawn Explore agents.\n";
    assert!(
        owned_claude_md_ranges(prose).is_empty(),
        "prose about tracedecay without a sentinel or shipped heading is operator text"
    );
    let dangling = format!("{}\n\nno end sentinel\n", CLAUDE_MD_SENTINELS.start);
    assert!(
        owned_claude_md_ranges(&dangling).is_empty(),
        "a start sentinel without its end is not an owned block"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn install_claude_md_rules_surfaces_lock_failures() {
    let err = install_claude_md_rules(Path::new("/dev/full")).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("failed to open host config lock /dev/"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn claude_prompt_install_rejects_non_utf8_without_overwrite() {
    let root = tempfile::tempdir().unwrap();
    let claude_md = root.path().join("CLAUDE.md");
    let invalid = b"operator rules\n\xff\xfe";
    std::fs::write(&claude_md, invalid).unwrap();

    let error = install_claude_md_rules(&claude_md).unwrap_err();

    assert!(error.to_string().contains("as UTF-8"), "{error}");
    assert_eq!(std::fs::read(&claude_md).unwrap(), invalid);
}

#[cfg(unix)]
#[test]
fn claude_prompt_install_rejects_unreadable_input_without_overwrite() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let claude_md = root.path().join("CLAUDE.md");
    std::fs::write(&claude_md, b"operator rules\n").unwrap();
    std::fs::set_permissions(&claude_md, std::fs::Permissions::from_mode(0o000)).unwrap();
    let error = install_claude_md_rules(&claude_md).unwrap_err();
    std::fs::set_permissions(&claude_md, std::fs::Permissions::from_mode(0o600)).unwrap();

    assert!(error.to_string().contains("failed to read"), "{error}");
    assert_eq!(std::fs::read(&claude_md).unwrap(), b"operator rules\n");
}

#[cfg(unix)]
#[test]
fn claude_prompt_install_refuses_a_symlink_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside.md");
    let claude_md = root.path().join("CLAUDE.md");
    std::fs::write(&outside, b"operator rules\n").unwrap();
    symlink(&outside, &claude_md).unwrap();

    let error = install_claude_md_rules(&claude_md).unwrap_err();

    assert!(
        error.to_string().contains("unsafe host metadata path"),
        "{error}"
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"operator rules\n");
}

#[test]
fn claude_uninstall_refuses_a_concurrent_edit_before_nonempty_rewrite() {
    let root = tempfile::tempdir().unwrap();
    let claude_md = root.path().join("CLAUDE.md");
    std::fs::write(&claude_md, b"operator rules\n").unwrap();
    install_claude_md_rules(&claude_md).unwrap();
    let pause = crate::agents::pause_next_host_config_write_at_publication(&claude_md);
    let writer_path = claude_md.clone();
    let remover = std::thread::spawn(move || {
        uninstall_claude_md_rules(&writer_path).map_err(|error| error.to_string())
    });
    pause.wait_until_reached();

    let foreign = b"foreign Claude edit\n";
    std::fs::write(&claude_md, foreign).unwrap();
    pause.resume();
    let error = remover.join().unwrap().unwrap_err();

    assert!(error.contains("changed since it was read"), "{error}");
    assert_eq!(std::fs::read(&claude_md).unwrap(), foreign);
}

#[test]
fn claude_uninstall_refuses_a_concurrent_edit_before_empty_deletion() {
    let root = tempfile::tempdir().unwrap();
    let claude_md = root.path().join("CLAUDE.md");
    install_claude_md_rules(&claude_md).unwrap();
    let pause = crate::agents::pause_next_host_config_write_at_publication(&claude_md);
    let writer_path = claude_md.clone();
    let remover = std::thread::spawn(move || {
        uninstall_claude_md_rules(&writer_path).map_err(|error| error.to_string())
    });
    pause.wait_until_reached();

    let foreign = b"foreign Claude edit\n";
    std::fs::write(&claude_md, foreign).unwrap();
    pause.resume();
    let error = remover.join().unwrap().unwrap_err();

    assert!(error.contains("changed since it was read"), "{error}");
    assert_eq!(std::fs::read(&claude_md).unwrap(), foreign);
}

#[test]
fn claude_uninstall_rewrites_operator_content_and_deletes_an_empty_result() {
    let root = tempfile::tempdir().unwrap();
    let nonempty = root.path().join("nonempty.md");
    std::fs::write(&nonempty, b"operator rules\n").unwrap();
    install_claude_md_rules(&nonempty).unwrap();

    uninstall_claude_md_rules(&nonempty).unwrap();

    assert_eq!(std::fs::read(&nonempty).unwrap(), b"operator rules\n");

    let empty = root.path().join("empty.md");
    install_claude_md_rules(&empty).unwrap();

    uninstall_claude_md_rules(&empty).unwrap();

    assert!(!empty.exists());
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
    deploy_rendered_bundle(home.path(), "/bin/tracedecay");
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
fn activation_reinstalls_a_same_version_cache_holding_an_older_build() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let claude = bin_dir.path().join("claude");
    deploy_rendered_bundle(home.path(), "/bin/tracedecay");
    write_native_activation(home.path(), "/bin/tracedecay");
    fake_claude_cli(&claude, &log, "exit 0");

    claude_plugin_activate_with(&claude, home.path())
        .expect("a current cache activates without an uninstall");
    let deploy = plugin_deploy_dir(home.path());
    let install = vec![
        format!("plugin marketplace add {}", deploy.display()),
        "plugin install tracedecay@tracedecay".to_string(),
    ];
    assert_eq!(recorded_invocations(&log), install);

    std::fs::remove_file(&log).unwrap();
    std::fs::write(
        claude_current_cached_plugin_root(home.path()).join(".mcp.json"),
        "{\"from\":\"an older build of the same version\"}\n",
    )
    .unwrap();
    claude_plugin_activate_with(&claude, home.path())
        .expect("a stale same-version cache is replaced through the host CLI");
    assert_eq!(
        recorded_invocations(&log),
        std::iter::once("plugin uninstall tracedecay".to_string())
            .chain(install)
            .collect::<Vec<_>>(),
        "`plugin install` skips an installed version, so the stale cache must be \
         uninstalled first"
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
        "the wildcard rule covers the namespace even with no per-tool list"
    );
}

#[test]
fn activation_adds_wildcard_permission_without_replacing_user_settings() {
    let home = tempfile::tempdir().unwrap();
    let tracedecay_bin = "/bin/tracedecay";
    deploy_rendered_bundle(home.path(), tracedecay_bin);
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
    safe_write_json_file(&settings_path, &existing).unwrap();
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: tracedecay_bin.to_string(),
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

#[test]
fn detected_host_surface_reports_claude_home() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(ClaudeIntegration.detected_host_surface(home.path()), None);
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    assert_eq!(
        ClaudeIntegration.detected_host_surface(home.path()),
        Some(home.path().join(".claude"))
    );
}

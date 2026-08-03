use super::*;
use serde_json::json;

fn plugin_source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugin")
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
    assert_eq!(
        plugin["version"].as_str().unwrap(),
        env!("TRACEDECAY_PRODUCT_VERSION")
    );

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

/// Running install twice must yield byte-identical config files.
#[test]
fn install_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let ctx = install_ctx(home.path());

    ClaudeIntegration.install(&ctx).unwrap();
    let read = |p: &Path| std::fs::read_to_string(p).ok();
    let settings_path = home.path().join(".claude/settings.json");
    let known_path = home.path().join(".claude/plugins/known_marketplaces.json");
    let plugin_path = home
        .path()
        .join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/plugin.json");
    let s1 = read(&settings_path);
    let k1 = read(&known_path);
    let p1 = read(&plugin_path);

    ClaudeIntegration.install(&ctx).unwrap();
    assert_eq!(s1, read(&settings_path), "settings.json must be stable");
    assert_eq!(
        k1,
        read(&known_path),
        "known_marketplaces.json must be stable"
    );
    assert_eq!(p1, read(&plugin_path), "plugin.json must be stable");
}

/// `register_marketplace` merges without clobbering existing marketplaces.
#[test]
fn register_marketplace_preserves_existing() {
    let home = tempfile::tempdir().unwrap();
    let path = known_marketplaces_path(home.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"claude-plugins-official":{"source":{"source":"github","repo":"x/y"}}}"#,
    )
    .unwrap();

    register_marketplace(home.path(), &plugin_deploy_dir(home.path())).unwrap();

    let known: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(known.get("claude-plugins-official").is_some());
    assert_eq!(
        known["tracedecay"]["source"]["source"].as_str().unwrap(),
        "directory"
    );
}

/// A settings.json whose `enabledPlugins`/`permissions` parents are the
/// wrong JSON type (a string / an array) must not panic install — the
/// guards coerce them to objects. Regression for `Value`'s `IndexMut`
/// panicking on a non-object parent.
#[test]
fn install_handles_malformed_settings_parents() {
    let home = tempfile::tempdir().unwrap();
    let claude_dir = home.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"enabledPlugins":"nope","permissions":[]}"#,
    )
    .unwrap();

    ClaudeIntegration
        .install(&install_ctx(home.path()))
        .expect("install must handle malformed settings parents gracefully");

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(settings["enabledPlugins"][PLUGIN_IDENTIFIER], json!(true));
    assert!(settings["permissions"]["allow"].is_array());
}

/// `enable_plugin` guards a non-object `enabledPlugins` parent.
#[test]
fn enable_plugin_coerces_non_object_parent() {
    let mut settings = json!({ "enabledPlugins": "garbage" });
    enable_plugin(&mut settings);
    assert_eq!(settings["enabledPlugins"][PLUGIN_IDENTIFIER], json!(true));
}

/// `install_permissions` guards a non-object `permissions` parent.
#[test]
fn install_permissions_coerces_non_object_parent() {
    let mut settings = json!({ "permissions": [] });
    install_permissions(&mut settings, &["mcp__tracedecay__search".to_string()]);
    assert!(settings["permissions"].is_object());
    let allow: Vec<&str> = settings["permissions"]["allow"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(allow.contains(&"mcp__tracedecay__search"));
    assert!(
        allow.contains(&"mcp__plugin_tracedecay_graph__search"),
        "legacy entries must gain their plugin-namespace twin"
    );
    let mut sorted = allow.clone();
    sorted.sort_unstable();
    assert_eq!(allow, sorted, "allowlist must be written sorted");
}

/// `enable_plugin` merges into existing `enabledPlugins` without dropping keys.
#[test]
fn enable_plugin_preserves_other_plugins() {
    let mut settings = json!({ "enabledPlugins": { "other@mkt": true } });
    enable_plugin(&mut settings);
    assert_eq!(settings["enabledPlugins"]["other@mkt"], json!(true));
    assert_eq!(settings["enabledPlugins"][PLUGIN_IDENTIFIER], json!(true));
}

/// Migration strips the loose MCP entry, the tracedecay hooks (all events),
/// and the loose subagents — but leaves non-tracedecay siblings intact.
#[test]
fn migration_removes_config_managed_but_keeps_foreign_entries() {
    let home = tempfile::tempdir().unwrap();
    let claude_dir = home.path().join(".claude");
    std::fs::create_dir_all(claude_dir.join("agents")).unwrap();

    std::fs::write(
        home.path().join(".claude.json"),
        r#"{"mcpServers":{"tracedecay":{"command":"tracedecay"},"other":{"command":"x"}}}"#,
    )
    .unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"hooks":{"Stop":[
                {"hooks":[{"type":"command","command":"tracedecay hook-stop"}]},
                {"hooks":[{"type":"command","command":"other-tool"}]}
            ]}}"#,
    )
    .unwrap();
    // A tracedecay-managed subagent plus a user file squatting on a name.
    std::fs::write(
        claude_dir.join("agents/code-explorer.md"),
        "managed tracedecay agent",
    )
    .unwrap();
    std::fs::write(
        claude_dir.join("agents/session-historian.md"),
        "my own agent, unrelated",
    )
    .unwrap();

    migrate_off_config_managed(home.path());

    let claude_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join(".claude.json")).unwrap())
            .unwrap();
    assert!(claude_json["mcpServers"].get("tracedecay").is_none());
    assert!(claude_json["mcpServers"].get("other").is_some());

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    let stop = settings["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 1, "only the foreign hook should survive");
    assert!(
        stop[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("other-tool")
    );

    assert!(
        !claude_dir.join("agents/code-explorer.md").exists(),
        "managed subagent removed"
    );
    assert!(
        claude_dir.join("agents/session-historian.md").exists(),
        "user subagent preserved"
    );
}

#[test]
fn migration_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let claude_dir = home.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        home.path().join(".claude.json"),
        r#"{"mcpServers":{"tracedecay":{"command":"tracedecay"}}}"#,
    )
    .unwrap();

    migrate_off_config_managed(home.path());
    let after_first = std::fs::read_to_string(home.path().join(".claude.json")).ok();
    migrate_off_config_managed(home.path());
    let after_second = std::fs::read_to_string(home.path().join(".claude.json")).ok();
    assert_eq!(after_first, after_second);
    assert!(!config_managed_mcp_present(home.path()));
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

    uninstall_claude_md_rules(&claude_md);

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
fn uninstall_permissions_removes_tracedecay_entries() {
    let mut settings = json!({
        "permissions": {
            "allow": [
                "Bash",
                "mcp__tracedecay__search",
                "mcp__tracedecay__lookup",
                "Read"
            ]
        }
    });
    let modified = uninstall_permissions(&mut settings);
    assert!(modified);
    let remaining: Vec<&str> = settings["permissions"]["allow"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(remaining, vec!["Bash", "Read"]);
}

#[test]
fn uninstall_removes_plugin_and_marketplace() {
    let home = tempfile::tempdir().unwrap();
    let ctx = install_ctx(home.path());
    ClaudeIntegration.install(&ctx).unwrap();
    assert!(plugin_marketplace_manifest_path(home.path()).exists());

    ClaudeIntegration.uninstall(&ctx).unwrap();
    assert!(
        !plugin_deploy_dir(home.path()).exists(),
        "deploy dir removed"
    );
    let known = known_marketplaces_path(home.path());
    if known.exists() {
        let val: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&known).unwrap()).unwrap();
        assert!(val.get(MARKETPLACE_NAME).is_none());
    }
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(
        settings
            .get("enabledPlugins")
            .and_then(|p| p.get(PLUGIN_IDENTIFIER))
            .is_none()
    );
}

#[test]
fn install_returns_contextual_error_when_claude_dir_is_not_a_directory() {
    let home = tempfile::tempdir().unwrap();
    let claude_path = home.path().join(".claude");
    std::fs::write(&claude_path, "not a directory").unwrap();

    let err = ClaudeIntegration
        .install(&install_ctx(home.path()))
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("failed to create Claude settings directory")
            && msg.contains(&claude_path.display().to_string()),
        "unexpected error message: {msg}"
    );
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
/// frontmatter and reference tracedecay so migration recognizes copies.
#[test]
fn managed_subagent_definitions_have_valid_frontmatter() {
    let files = claude_embedded_plugin_files();
    for &file_name in LEGACY_SUBAGENT_FILES {
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

use super::*;

/// The repo-local `hooks-codex.json` ships an empty `hooks` object plus a
/// self-documenting `description`. Rendering the global bundle must fill the
/// object from `CODEX_MANAGED_HOOKS` while leaving the description intact,
/// and must never invent hooks the managed table does not declare.
#[test]
fn codex_plugin_hooks_fills_empty_seed_and_preserves_description() {
    let raw = codex_embedded_plugin_files()
        .into_iter()
        .find_map(|(relative, contents)| (relative == "hooks/hooks.json").then_some(contents))
        .expect("codex bundle ships hooks/hooks.json");

    // The seed template is genuinely empty (it is not dead weight: it is the
    // base the renderer mutates in place).
    let seed: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(seed["hooks"], json!({}));
    assert!(
        seed["description"]
            .as_str()
            .unwrap()
            .contains("no lifecycle hooks"),
        "empty seed must carry a self-documenting description"
    );

    let rendered = codex_plugin_hooks(raw, "/usr/local/bin/tracedecay").unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    // The description survives rendering (Codex's loader ignores it).
    assert_eq!(value["description"], seed["description"]);
    let hooks = value["hooks"].as_object().unwrap();
    for managed in CODEX_MANAGED_HOOKS {
        assert!(
            hooks.contains_key(managed.event),
            "rendered global bundle missing managed event {}",
            managed.event
        );
    }
    assert_eq!(
        hooks.len(),
        CODEX_MANAGED_HOOKS.len(),
        "rendered bundle must register exactly the managed hooks"
    );
}

#[test]
fn native_memories_injection_detection_covers_config_shapes() {
    let parse = |raw: &str| toml::from_str::<toml::Value>(raw).unwrap();
    // Feature on (bool form), use_memories defaulting to true.
    assert!(codex_native_memories_injection_enabled(&parse(
        "[features]\nmemories = true\n"
    )));
    // Feature on (nested table form).
    assert!(codex_native_memories_injection_enabled(&parse(
        "[features.memories]\ncustom_tools = true\n"
    )));
    // Injection explicitly disabled.
    assert!(!codex_native_memories_injection_enabled(&parse(
        "[features]\nmemories = true\n[memories]\nuse_memories = false\n"
    )));
    // Feature off or absent.
    assert!(!codex_native_memories_injection_enabled(&parse(
        "[features]\nmemories = false\n"
    )));
    assert!(!codex_native_memories_injection_enabled(&parse("")));
}

#[test]
fn codex_hook_trust_state_reports_all_trusted_entries() {
    let config = r#"
[hooks.state]

[hooks.state."tracedecay@personal:hooks/hooks.json:post_tool_use:0:0"]
trusted_hash = "sha256:post"

[hooks.state."tracedecay@personal:hooks/hooks.json:session_start:0:0"]
trusted_hash = "sha256:session"

[hooks.state."tracedecay@personal:hooks/hooks.json:user_prompt_submit:0:0"]
trusted_hash = "sha256:prompt"

[hooks.state."tracedecay@personal:hooks/hooks.json:subagent_start:0:0"]
trusted_hash = "sha256:subagent"

[hooks.state."tracedecay@personal:hooks/hooks.json:post_compact:0:0"]
trusted_hash = "sha256:compact"
"#;
    let config = toml::from_str::<toml::Value>(config).unwrap();

    assert_eq!(
        codex_plugin_hook_trust_state(&config),
        CodexHookTrustState::Trusted
    );
}

#[test]
fn codex_hook_trust_state_reports_missing_entries() {
    let config = toml::from_str::<toml::Value>(
        r#"
[hooks.state]

[hooks.state."tracedecay@personal:hooks/hooks.json:post_tool_use:0:0"]
trusted_hash = "sha256:post"
"#,
    )
    .unwrap();

    assert_eq!(
        codex_plugin_hook_trust_state(&config),
        CodexHookTrustState::Missing(vec![
            "session_start".to_string(),
            "user_prompt_submit".to_string(),
            "subagent_start".to_string(),
            "post_compact".to_string(),
        ])
    );
}

#[test]
fn codex_hook_trust_state_ignores_repo_local_plugin_entries() {
    let config = toml::from_str::<toml::Value>(
        r#"
[hooks.state]

[hooks.state."tracedecay@local-repo:hooks/hooks.json:post_tool_use:0:0"]
trusted_hash = "sha256:post"

[hooks.state."tracedecay@local-repo:hooks/hooks.json:session_start:0:0"]
trusted_hash = "sha256:session"

[hooks.state."tracedecay@local-repo:hooks/hooks.json:user_prompt_submit:0:0"]
trusted_hash = "sha256:prompt"

[hooks.state."tracedecay@local-repo:hooks/hooks.json:subagent_start:0:0"]
trusted_hash = "sha256:subagent"

[hooks.state."tracedecay@local-repo:hooks/hooks.json:post_compact:0:0"]
trusted_hash = "sha256:compact"
"#,
    )
    .unwrap();

    assert_eq!(
        codex_plugin_hook_trust_state(&config),
        CodexHookTrustState::Missing(vec![
            "session_start".to_string(),
            "user_prompt_submit".to_string(),
            "subagent_start".to_string(),
            "post_tool_use".to_string(),
            "post_compact".to_string(),
        ])
    );
}

#[test]
fn codex_legacy_mcp_detector_ignores_plugin_entries() {
    let home = tempfile::tempdir().expect("tempdir should create");
    let codex_dir = home.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).expect("codex dir should create");
    std::fs::write(
        codex_dir.join("config.toml"),
        r#"
[plugins."tracedecay@personal"]
enabled = true

[hooks.state."tracedecay@personal:hooks/hooks.json:post_tool_use:0:0"]
trusted_hash = "sha256:post"
"#,
    )
    .expect("config should write");

    assert!(
        !codex_legacy_config_has_tracedecay(home.path()),
        "plugin and hook entries are not legacy direct MCP config"
    );
}

#[test]
fn codex_legacy_mcp_detector_finds_direct_mcp_config() {
    let home = tempfile::tempdir().expect("tempdir should create");
    let codex_dir = home.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).expect("codex dir should create");
    std::fs::write(
        codex_dir.join("config.toml"),
        r#"
[mcp_servers.tracedecay]
command = "/old/bin/tracedecay"
args = ["serve"]
"#,
    )
    .expect("config should write");

    assert!(codex_legacy_config_has_tracedecay(home.path()));
}

#[test]
fn remove_legacy_codex_native_automation_deletes_stale_record() {
    let home = tempfile::tempdir().expect("tempdir should create");
    assert!(
        !remove_legacy_codex_native_automation(home.path())
            .expect("removal without a record should succeed"),
        "no legacy record should report nothing removed"
    );

    let automation_dir = home
        .path()
        .join(".codex/automations")
        .join(LEGACY_CODEX_NATIVE_AUTOMATION_ID);
    std::fs::create_dir_all(&automation_dir).expect("legacy dir should create");
    std::fs::write(
        automation_dir.join("automation.toml"),
        "status = \"ACTIVE\"\n",
    )
    .expect("legacy automation should write");

    assert!(
        remove_legacy_codex_native_automation(home.path())
            .expect("removal of an existing record should succeed"),
        "an existing legacy record should report removal"
    );
    assert!(
        !automation_dir.exists(),
        "the legacy automation directory should be gone"
    );
}

/// The composed Codex deploy set (sourced from the shared `plugin/` tree
/// via `codex_files`) must cover every shared model-invocable skill and the
/// 13 canonical `tracedecay-*` workflow dispatchers, plus Codex's manifest,
/// `.mcp.json`, hooks, and README. Codex has no slash-command or
/// `disable-model-invocation` surface, so it ships all 29 skills in their
/// canonical (model-invocable) form. The single shared tree means there is
/// no cross-bundle parity to enforce anymore — this replaces the old
/// `codex_skills_match_the_cursor_source_for_parity` /
/// `codex_bundle_ships_exactly_the_model_invocable_cursor_skills` checks.
/// Every file under a skills root, relative to it, forward-slashed.
fn skill_tree_files(root: &Path) -> Vec<String> {
    let mut files: Vec<String> = crate::agents::collect_regular_files(root)
        .expect("skills dir readable")
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(root)
                .ok()
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        })
        .collect();
    files.sort();
    files
}

#[test]
fn codex_embedded_file_list_covers_the_whole_source_bundle() {
    let deploy: std::collections::BTreeSet<String> = codex_embedded_plugin_files()
        .into_iter()
        .map(|(relative, _)| relative.to_string())
        .collect();

    // Every skill dir under plugin/skills is deployed by Codex (all 13).
    let skills_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin/skills");
    let mut skill_dirs: Vec<String> = std::fs::read_dir(&skills_root)
        .expect("plugin/skills should be readable")
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    skill_dirs.sort();
    assert_eq!(skill_dirs.len(), 13, "expected 13 shared skill dirs");
    // Every file under plugin/skills/ (SKILL.md *and* any support files) is
    // deployed — the recursive embed leaves nothing on disk unwired.
    for relative in skill_tree_files(&skills_root) {
        let expected = format!("skills/{relative}");
        assert!(
            deploy.contains(&expected),
            "Codex deploy set is missing skill file {expected}"
        );
    }

    // Codex's manifest surfaces.
    for expected in [
        ".codex-plugin/plugin.json",
        ".mcp.json",
        "hooks/hooks.json",
        "README.md",
    ] {
        assert!(
            deploy.contains(expected),
            "Codex deploy set is missing {expected}"
        );
    }
}

/// Extracts the `<name>` from every `tracedecay:<name>` skill handoff in a
/// body. MCP tool calls use `tracedecay_*` (underscore) and are ignored.
fn skill_handoff_references(body: &str) -> Vec<String> {
    const MARKER: &str = "tracedecay:";
    let mut refs = Vec::new();
    let mut rest = body;
    while let Some(pos) = rest.find(MARKER) {
        rest = &rest[pos + MARKER.len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
            .collect();
        if !name.is_empty() {
            refs.push(name);
        }
    }
    refs
}

/// Every `tracedecay:<skill>` handoff inside the embedded Codex skill bodies
/// must resolve to a skill this bundle actually ships. A dangling reference
/// (e.g. to a Cursor-only explicit-invoke skill)
/// would point a Codex agent at a workflow that does not exist here.
#[test]
fn codex_skill_cross_references_resolve_to_shipped_skills() {
    let files = codex_embedded_plugin_files();
    let shipped: std::collections::BTreeSet<String> = files
        .iter()
        .filter_map(|&(relative, _)| {
            relative
                .strip_prefix("skills/")
                .and_then(|rest| rest.strip_suffix("/SKILL.md"))
                .map(str::to_string)
        })
        .collect();

    let mut dangling: Vec<String> = Vec::new();
    for &(relative, contents) in &files {
        if !relative.starts_with("skills/") {
            continue;
        }
        for reference in skill_handoff_references(contents) {
            if !shipped.contains(&reference) {
                dangling.push(format!("{relative} -> tracedecay:{reference}"));
            }
        }
    }
    assert!(
        dangling.is_empty(),
        "Codex skill bodies reference skills absent from the bundle: {dangling:?}"
    );
}

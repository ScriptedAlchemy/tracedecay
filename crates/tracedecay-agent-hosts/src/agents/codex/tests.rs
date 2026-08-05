use super::*;

/// The repo-local `hooks-codex.json` ships only an empty `hooks` object.
/// Rendering the global bundle must fill the object from `CODEX_MANAGED_HOOKS`
/// while keeping Codex's strict top-level schema clean.
#[test]
fn codex_plugin_hooks_fills_empty_seed_and_preserves_strict_schema() {
    let raw = codex_embedded_plugin_files()
        .into_iter()
        .find_map(|(relative, contents)| (relative == "hooks/hooks.json").then_some(contents))
        .expect("codex bundle ships hooks/hooks.json");

    // The seed template is genuinely empty (it is not dead weight: it is the
    // base the renderer mutates in place).
    let seed: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(seed["hooks"], json!({}));
    assert_eq!(
        seed.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["hooks"],
        "Codex rejects unknown top-level hook fields"
    );

    let rendered = codex_plugin_hooks(raw, "/usr/local/bin/tracedecay").unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    let top_level_keys = value.as_object().unwrap().keys().collect::<Vec<_>>();
    assert_eq!(
        top_level_keys,
        vec!["hooks"],
        "rendered hooks bundle must stay within Codex's strict schema"
    );
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

/// A stable binary path for hashing tests. The real trust hash depends only on
/// the rendered command string, so any fixed path yields deterministic hashes.
const TEST_BIN: &str = "/usr/local/bin/tracedecay";

/// Render the personal-bundle hooks and derive their trust records with `bin`.
fn managed_entries(bin: &str) -> Vec<CodexHookTrustEntry> {
    codex_managed_hook_trust_entries(bin).expect("managed hook trust entries render")
}

/// Build a `config.toml` value whose `[hooks.state]` records exactly the given
/// trust entries (each as `trusted_hash = <entry.hash>`).
fn config_from_entries(entries: &[CodexHookTrustEntry]) -> toml::Value {
    let mut state = toml::value::Table::new();
    for entry in entries {
        let mut record = toml::value::Table::new();
        record.insert(
            "trusted_hash".to_string(),
            toml::Value::String(entry.hash.clone()),
        );
        state.insert(entry.trust_key.clone(), toml::Value::Table(record));
    }
    let mut hooks = toml::value::Table::new();
    hooks.insert("state".to_string(), toml::Value::Table(state));
    let mut root = toml::value::Table::new();
    root.insert("hooks".to_string(), toml::Value::Table(hooks));
    toml::Value::Table(root)
}

/// The five live-trusted golden hashes verified byte-for-byte against a real
/// Codex `~/.codex/config.toml` on the reference machine. The hash function must
/// reproduce each from its raw command-hook identity, or the installer would
/// record trust Codex rejects.
#[test]
fn codex_command_hook_hash_reproduces_live_golden_vectors() {
    let cmd = |sub: &str| format!("'/home/zack/.local/bin/tracedecay' {sub}");
    let cases = [
        (
            "session_start",
            None,
            cmd("hook-codex-session-start"),
            5u64,
            "sha256:839cc2cfa576115dfa9e184eb267eb5bd565750c20babcb2d0358c68ec7c5c42",
        ),
        (
            "post_tool_use",
            Some("Bash|apply_patch"),
            cmd("hook-codex-post-tool-use"),
            60,
            "sha256:9dd11f4b944d2b9b8f14d4f17ca8a52e1550e575d3087177ec42d7c7f8848c97",
        ),
        (
            "user_prompt_submit",
            None,
            cmd("hook-codex-user-prompt-submit"),
            5,
            "sha256:d482382b39ab1f031943d27359c8626b36ebfff66259468377fffcd7174e9313",
        ),
        (
            "subagent_start",
            None,
            cmd("hook-codex-subagent-start"),
            5,
            "sha256:4042991d127afeef0452f5b9a3fed48b48596e1b6de114b7e3392764f1c467ab",
        ),
        (
            "post_compact",
            Some("auto|manual"),
            cmd("hook-codex-post-compact"),
            120,
            "sha256:85ce51c00b972536033286d8d8489dbb396dd1ea97bd2a4f10dbaf7aa39a0764",
        ),
    ];
    for (event, matcher, command, timeout, expected) in cases {
        assert_eq!(
            codex_command_hook_hash(event, matcher, &command, timeout, false).unwrap(),
            expected,
            "hash mismatch for {event}"
        );
    }
}

#[test]
fn codex_command_hook_hash_propagates_canonicalization_failure() {
    let error = codex_command_hook_hash_with("session_start", None, TEST_BIN, 5, false, |_| {
        Err("forced canonicalization failure".to_string())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        TraceDecayError::Config { message }
            if message.contains("forced canonicalization failure")
    ));
}

#[test]
fn codex_hook_trust_state_reports_all_trusted_entries() {
    let entries = managed_entries(TEST_BIN);
    let config = config_from_entries(&entries);

    assert_eq!(
        codex_plugin_hook_trust_state(&config, &entries),
        CodexHookTrustState::Trusted
    );
}

#[test]
fn codex_hook_trust_state_reports_missing_entries() {
    let entries = managed_entries(TEST_BIN);
    // Record trust for only the post_tool_use hook; the rest are missing.
    let present: Vec<CodexHookTrustEntry> = entries
        .iter()
        .filter(|entry| entry.event_label == "post_tool_use")
        .cloned()
        .collect();
    let config = config_from_entries(&present);

    assert_eq!(
        codex_plugin_hook_trust_state(&config, &entries),
        CodexHookTrustState::Missing(vec![
            "post_compact".to_string(),
            "session_start".to_string(),
            "stop".to_string(),
            "subagent_start".to_string(),
            "user_prompt_submit".to_string(),
        ])
    );
}

#[test]
fn codex_hook_trust_state_flags_modified_when_hash_drifts() {
    let entries = managed_entries(TEST_BIN);
    // Simulate a bundle change: bump one hook's timeout so its content hash
    // drifts from what was previously trusted.
    let raw = codex_embedded_plugin_files()
        .into_iter()
        .find_map(|(relative, contents)| (relative == "hooks/hooks.json").then_some(contents))
        .unwrap();
    let rendered = codex_plugin_hooks(raw, TEST_BIN).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    value["hooks"]["SessionStart"][0]["hooks"][0]["timeout"] = json!(9);
    let changed_entries = codex_hook_trust_entries(&value).unwrap();

    // config still records the *original* hashes; against the changed bundle,
    // only session_start drifts.
    let config = config_from_entries(&entries);
    assert_eq!(
        codex_plugin_hook_trust_state(&config, &changed_entries),
        CodexHookTrustState::Modified(vec!["session_start".to_string()])
    );

    // Re-syncing to the changed bundle restores Trusted.
    let resynced = config_from_entries(&changed_entries);
    assert_eq!(
        codex_plugin_hook_trust_state(&resynced, &changed_entries),
        CodexHookTrustState::Trusted
    );
}

#[test]
fn codex_hook_trust_state_ignores_repo_local_plugin_entries() {
    let entries = managed_entries(TEST_BIN);
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

[hooks.state."tracedecay@local-repo:hooks/hooks.json:stop:0:0"]
trusted_hash = "sha256:stop"
"#,
    )
    .unwrap();

    assert_eq!(
        codex_plugin_hook_trust_state(&config, &entries),
        CodexHookTrustState::Missing(vec![
            "post_compact".to_string(),
            "post_tool_use".to_string(),
            "session_start".to_string(),
            "stop".to_string(),
            "subagent_start".to_string(),
            "user_prompt_submit".to_string(),
        ])
    );
}

#[test]
fn codex_marketplace_identity_rejects_path_and_trust_key_injection() {
    let home = tempfile::tempdir().expect("tempdir");
    let marketplace_path = codex_personal_marketplace_path(home.path());
    std::fs::create_dir_all(marketplace_path.parent().unwrap()).unwrap();

    for unsafe_name in [
        "../escape",
        "/absolute",
        r"parent\child",
        "name:hooks",
        "line\nbreak",
    ] {
        std::fs::write(
            &marketplace_path,
            serde_json::json!({"name": unsafe_name, "plugins": []}).to_string(),
        )
        .unwrap();
        let err = codex_personal_marketplace_name(home.path()).unwrap_err();
        assert!(
            err.to_string().contains("safe ASCII path segment"),
            "unsafe marketplace name {unsafe_name:?} produced {err}"
        );
        assert_eq!(
            codex_cached_marketplace_name(home.path()),
            CODEX_DEFAULT_MARKETPLACE_NAME,
            "an unsafe marketplace identity must not influence cache paths"
        );
    }
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

    // Every skill dir under plugin/skills is deployed by Codex (all 14).
    let skills_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin/skills");
    let mut skill_dirs: Vec<String> = std::fs::read_dir(&skills_root)
        .expect("plugin/skills should be readable")
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    skill_dirs.sort();
    assert_eq!(skill_dirs.len(), 15, "expected 15 shared skill dirs");
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

fn install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: TEST_BIN.to_string(),
        tool_permissions: Vec::new(),
        project_root: None,
        dashboard: false,
    }
}

fn copy_rendered_bundle_to_native_cache(home: &Path, tracedecay_bin: &str) {
    let source = codex_plugin_install_dir(home);
    let cache = codex_plugin_current_cached_install_dir(home);
    for (relative, _) in rendered_global_plugin_files(tracedecay_bin).unwrap() {
        let target = cache.join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(source.join(relative), target).unwrap();
    }
}

fn write_exact_native_activation(home: &Path, tracedecay_bin: &str) {
    install_codex_personal_bootstrap(home, tracedecay_bin).unwrap();
    let config = codex_config_path(home);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        "[plugins.\"tracedecay@personal\"]\nenabled = true\n",
    )
    .unwrap();
    copy_rendered_bundle_to_native_cache(home, tracedecay_bin);
}

#[test]
fn native_activation_binds_enabled_key_to_exact_marketplace_and_cache() {
    let home = tempfile::tempdir().unwrap();
    write_exact_native_activation(home.path(), TEST_BIN);
    assert_eq!(
        codex_plugin_install_dir(home.path()),
        home.path().join(".codex/plugins/tracedecay")
    );
    assert!(!home.path().join("plugins/tracedecay").exists());
    let marketplace: serde_json::Value = serde_json::from_slice(
        &std::fs::read(codex_personal_marketplace_path(home.path())).unwrap(),
    )
    .unwrap();
    assert_eq!(
        marketplace
            .pointer("/plugins/0/source/path")
            .and_then(serde_json::Value::as_str),
        Some("./.codex/plugins/tracedecay")
    );
    assert!(codex_plugin_activation_state(home.path(), Some(TEST_BIN)).unwrap());

    std::fs::write(
        codex_config_path(home.path()),
        "[plugins.\"tracedecay@other\"]\nenabled = true\n",
    )
    .unwrap();
    assert!(!codex_plugin_activation_state(home.path(), Some(TEST_BIN)).unwrap());
}

#[test]
fn native_activation_rejects_cache_from_another_marketplace() {
    let home = tempfile::tempdir().unwrap();
    write_exact_native_activation(home.path(), TEST_BIN);
    let exact = codex_plugin_current_cached_install_dir(home.path());
    let other = codex_plugin_cached_root(home.path(), "other").join(crate::PRODUCT_VERSION);
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    std::fs::rename(exact, other).unwrap();

    assert!(!codex_plugin_activation_state(home.path(), Some(TEST_BIN)).unwrap());
}

#[test]
fn native_activation_rejects_marketplace_source_path_drift() {
    let home = tempfile::tempdir().unwrap();
    write_exact_native_activation(home.path(), TEST_BIN);
    let marketplace_path = codex_personal_marketplace_path(home.path());
    let mut marketplace: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marketplace_path).unwrap()).unwrap();
    marketplace["plugins"][0]["source"]["path"] = json!("./plugins/other");
    std::fs::write(
        marketplace_path,
        serde_json::to_vec_pretty(&marketplace).unwrap(),
    )
    .unwrap();

    assert!(!codex_plugin_activation_state(home.path(), Some(TEST_BIN)).unwrap());
}

#[test]
fn native_cache_content_drift_and_binary_relocation_require_refresh() {
    let home = tempfile::tempdir().unwrap();
    let old_bin = "/old/bin/tracedecay";
    let new_bin = "/relocated/bin/tracedecay";
    write_exact_native_activation(home.path(), old_bin);
    let old_ctx = install_ctx(home.path());
    let old_ctx = InstallContext {
        tracedecay_bin: old_bin.to_string(),
        ..old_ctx
    };
    assert!(matches!(
        CodexIntegration
            .preflight_non_interactive_install(&old_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::Ready
    ));

    let retired_skill =
        codex_plugin_current_cached_install_dir(home.path()).join("skills/retired/SKILL.md");
    std::fs::create_dir_all(retired_skill.parent().unwrap()).unwrap();
    std::fs::write(&retired_skill, "# stale auto-discovered skill\n").unwrap();
    assert!(matches!(
        CodexIntegration
            .preflight_non_interactive_install(&old_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::DeferredUserAction(_)
    ));
    std::fs::remove_file(retired_skill).unwrap();
    assert!(matches!(
        CodexIntegration
            .preflight_non_interactive_install(&old_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::Ready
    ));

    std::fs::write(
        codex_plugin_current_cached_install_dir(home.path()).join(".mcp.json"),
        "{}\n",
    )
    .unwrap();
    assert!(matches!(
        CodexIntegration
            .preflight_non_interactive_install(&old_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::DeferredUserAction(_)
    ));
    copy_rendered_bundle_to_native_cache(home.path(), old_bin);
    assert!(matches!(
        CodexIntegration
            .preflight_non_interactive_install(&old_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::Ready
    ));

    install_codex_personal_bootstrap(home.path(), new_bin).unwrap();
    let relocated_ctx = InstallContext {
        tracedecay_bin: new_bin.to_string(),
        ..old_ctx
    };
    assert!(matches!(
        CodexIntegration
            .preflight_non_interactive_install(&relocated_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::DeferredUserAction(_)
    ));
    copy_rendered_bundle_to_native_cache(home.path(), new_bin);
    assert!(matches!(
        CodexIntegration
            .preflight_non_interactive_install(&relocated_ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::Ready
    ));
}

#[test]
fn redeploy_retires_owned_discovery_files_and_preserves_foreign_bytes() {
    const RETIRED_FIND_IMPACT_SKILL: &str = r#"---
name: tracedecay-find-impact
description: 'Use to find the blast radius of a change, including impacted symbols, files, and the tests to run.'
---

# Find impact

Use to find a change's blast radius: impacted symbols, files, and tests to run.

Use `tracedecay:assessing-impact`.

- **Target:** the symbol, file, or change to analyze. If none is given, use the current working-tree diff.
- Read-only: shallow `max_depth` first. Identify impact; do not run tests.

Output: impacted symbols + files, the test set to run, and any hub/coupling risk.
"#;

    let home = tempfile::tempdir().unwrap();
    write_exact_native_activation(home.path(), TEST_BIN);
    let ctx = install_ctx(home.path());
    let source = codex_plugin_install_dir(home.path());
    let retired = source.join("skills/tracedecay-find-impact/SKILL.md");
    std::fs::create_dir_all(retired.parent().unwrap()).unwrap();
    std::fs::write(&retired, RETIRED_FIND_IMPACT_SKILL).unwrap();
    let operator_support = retired.parent().unwrap().join("operator-notes.txt");
    let operator_support_bytes = b"preserve operator TraceDecay MCP support bytes";
    std::fs::write(&operator_support, operator_support_bytes).unwrap();
    let reference = retired.parent().unwrap().join("reference.md");
    let reference_bytes = b"Operator reference for tracedecay_message_search";
    std::fs::write(&reference, reference_bytes).unwrap();
    let helper = source.join("hooks/helper.py");
    let helper_bytes = b"# operator helper for tracedecay_lcm_describe\n";
    std::fs::write(&helper, helper_bytes).unwrap();
    assert!(matches!(
        CodexIntegration
            .preflight_non_interactive_install(&ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::DeferredUserAction(_)
    ));

    install_codex_personal_bootstrap(home.path(), TEST_BIN).unwrap();
    assert!(!retired.exists());
    assert_eq!(
        std::fs::read(&operator_support).unwrap(),
        operator_support_bytes
    );
    assert_eq!(std::fs::read(&reference).unwrap(), reference_bytes);
    assert_eq!(std::fs::read(&helper).unwrap(), helper_bytes);
    assert!(matches!(
        CodexIntegration
            .preflight_non_interactive_install(&ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::Ready
    ));

    let foreign = source.join("skills/operator-owned/SKILL.md");
    std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
    let foreign_bytes = b"---\nname: operator-owned\ndescription: Use the TraceDecay MCP safely\n---\n\nCall `tracedecay_context` for indexed code.\n";
    std::fs::write(&foreign, foreign_bytes).unwrap();
    install_codex_personal_bootstrap(home.path(), TEST_BIN).unwrap();
    assert_eq!(std::fs::read(foreign).unwrap(), foreign_bytes);
    assert!(matches!(
        CodexIntegration
            .preflight_non_interactive_install(&ctx)
            .unwrap(),
        NonInteractiveInstallOutcome::DeferredUserAction(_)
    ));
}

/// Codex cache activation is intentionally deferred to the host CLI.
#[test]
fn codex_reports_host_native_activation_requirement() {
    let home = tempfile::tempdir().unwrap();
    let NonInteractiveInstallOutcome::DeferredUserAction(deferred) = CodexIntegration
        .preflight_non_interactive_install(&install_ctx(home.path()))
        .unwrap()
    else {
        panic!("Codex activation must defer to its native cache lifecycle");
    };
    assert!(
        deferred
            .remediation
            .contains("codex plugin add tracedecay@personal")
    );
    assert!(CodexIntegration.interactive_activation_guidance().is_some());
}

#[test]
fn codex_update_stages_source_without_overwriting_native_cache_or_config() {
    let home = tempfile::tempdir().unwrap();
    install_codex_personal_bootstrap(home.path(), TEST_BIN).unwrap();

    let cache_file = codex_plugin_current_cached_install_dir(home.path()).join("user-cache.txt");
    std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
    std::fs::write(&cache_file, "native cache bytes").unwrap();
    let config_path = codex_config_path(home.path());
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "model = \"user-choice\"\n").unwrap();

    let outcome = CodexIntegration
        .update_plugin(&install_ctx(home.path()))
        .unwrap();
    let UpdatePluginOutcome::DeferredUserAction(deferred) = outcome else {
        panic!("Codex refresh must defer to its native plugin lifecycle");
    };
    assert!(
        deferred
            .remediation
            .contains("codex plugin update tracedecay@personal")
    );
    assert_eq!(
        std::fs::read_to_string(&cache_file).unwrap(),
        "native cache bytes"
    );
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        "model = \"user-choice\"\n"
    );
}

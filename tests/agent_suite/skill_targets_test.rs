use tracedecay::agents::codex::export_codex_plugin_artifact;
use tracedecay::agents::{export_managed_skills_to_agent_hosts, export_managed_skills_to_agents};
use tracedecay_agent_hosts::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSupportFile,
    create_managed_skill, default_managed_skill_targets, disable_managed_skill, load_managed_skill,
    managed_skill_dir,
};
use tracedecay_agent_hosts::automation::skill_targets::{
    SkillInstallTarget, export_native_skill_overlay, export_prompt_skill_index,
    install_managed_skills, remove_prompt_skill_index, remove_prompt_skill_index_for_target,
};

fn draft(id: &str, title: &str) -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: id.to_string(),
        title: title.to_string(),
        summary: format!("{title} summary"),
        category: "workflow".to_string(),
        targets: default_managed_skill_targets(),
        body_markdown: format!("Use {title} when the workflow repeats."),
        support_files: vec![
            ManagedSupportFile::new("references/checklist.md", format!("- {id}\n").into_bytes())
                .unwrap(),
        ],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::User,
            actor: "test".to_string(),
            run_id: None,
        },
    }
}

fn targeted_draft(id: &str, title: &str, targets: Vec<SkillInstallTarget>) -> ManagedSkillDraft {
    ManagedSkillDraft {
        targets,
        ..draft(id, title)
    }
}

#[test]
fn managed_skill_defaults_target_supported_hosts() {
    let targets = default_managed_skill_targets();
    assert_eq!(
        targets,
        vec![
            SkillInstallTarget::Cursor,
            SkillInstallTarget::Codex,
            SkillInstallTarget::Claude,
            SkillInstallTarget::Agents,
            SkillInstallTarget::OpenCode,
            SkillInstallTarget::Kimi,
            SkillInstallTarget::Kiro,
            SkillInstallTarget::Hermes,
        ]
    );
}

#[tokio::test]
async fn native_overlay_exports_automatically_active_skills_and_prunes_generated_namespace() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let plugin_root = temp.path().join("cursor-plugin");

    create_managed_skill(
        &profile_root,
        targeted_draft(
            "repo-hygiene",
            "Repository hygiene",
            vec![SkillInstallTarget::Cursor],
        ),
    )
    .await
    .unwrap();
    create_managed_skill(
        &profile_root,
        targeted_draft(
            "claude-flow",
            "Claude flow",
            vec![SkillInstallTarget::Claude],
        ),
    )
    .await
    .unwrap();

    std::fs::create_dir_all(plugin_root.join("skills/static-skill")).unwrap();
    std::fs::write(
        plugin_root.join("skills/static-skill/SKILL.md"),
        "static bundle skill",
    )
    .unwrap();
    std::fs::create_dir_all(plugin_root.join("skills/agent-managed/stale-skill")).unwrap();
    std::fs::write(
        plugin_root.join("skills/agent-managed/stale-skill/SKILL.md"),
        "stale generated skill",
    )
    .unwrap();

    let summary =
        export_native_skill_overlay(&profile_root, SkillInstallTarget::Cursor, &plugin_root)
            .unwrap();

    assert_eq!(summary.exported_count, 1);
    assert!(
        plugin_root
            .join("skills/agent-managed/repo-hygiene/SKILL.md")
            .is_file()
    );
    assert!(
        plugin_root
            .join("skills/agent-managed/repo-hygiene/references/checklist.md")
            .is_file()
    );
    assert!(
        !plugin_root
            .join("skills/agent-managed/claude-flow/SKILL.md")
            .exists()
    );
    assert!(
        !plugin_root
            .join("skills/agent-managed/stale-skill/SKILL.md")
            .exists()
    );
    assert!(plugin_root.join("skills/static-skill/SKILL.md").is_file());
    assert!(
        plugin_root
            .join("skills/agent-managed/.tracedecay-managed-skills.json")
            .is_file()
    );
    let manifest = std::fs::read_to_string(
        plugin_root.join("skills/agent-managed/.tracedecay-managed-skills.json"),
    )
    .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let exported_path = manifest["exported"][0]["path"].as_str().unwrap();
    let exported_path_normalized = exported_path.replace('\\', "/");
    assert!(exported_path_normalized.ends_with("skills/agent-managed/repo-hygiene/SKILL.md"));
    assert!(
        !exported_path.contains(".agent-managed.tmp"),
        "manifest must expose final overlay paths: {exported_path}"
    );

    disable_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let summary =
        export_native_skill_overlay(&profile_root, SkillInstallTarget::Cursor, &plugin_root)
            .unwrap();
    assert_eq!(summary.exported_count, 0);
    assert!(
        !plugin_root
            .join("skills/agent-managed/repo-hygiene/SKILL.md")
            .exists()
    );
    assert!(plugin_root.join("skills/static-skill/SKILL.md").is_file());
}

#[tokio::test]
async fn codex_native_overlay_uses_agent_managed_namespace() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let plugin_root = temp.path().join("codex-plugin");

    create_managed_skill(
        &profile_root,
        targeted_draft(
            "repo-hygiene",
            "Repository hygiene",
            vec![SkillInstallTarget::Codex],
        ),
    )
    .await
    .unwrap();

    let summary =
        export_native_skill_overlay(&profile_root, SkillInstallTarget::Codex, &plugin_root)
            .unwrap();
    assert_eq!(summary.exported_count, 1);
    assert!(
        plugin_root
            .join("skills/agent-managed/repo-hygiene/SKILL.md")
            .is_file()
    );
}

#[tokio::test]
async fn codex_plugin_artifact_exports_shareable_bundle_with_managed_skills() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let plugin_root = temp.path().join("codex-plugin");

    create_managed_skill(
        &profile_root,
        targeted_draft("codex-only", "Codex only", vec![SkillInstallTarget::Codex]),
    )
    .await
    .unwrap();
    create_managed_skill(
        &profile_root,
        targeted_draft(
            "cursor-only",
            "Cursor only",
            vec![SkillInstallTarget::Cursor],
        ),
    )
    .await
    .unwrap();

    let summary = export_codex_plugin_artifact(&profile_root, &plugin_root, "tracedecay-bin")
        .expect("Codex plugin artifact should export");

    assert_eq!(summary.exported_count, 1);
    assert_eq!(summary.exported[0].id, "codex-only");
    assert!(plugin_root.join(".codex-plugin/plugin.json").is_file());
    assert!(plugin_root.join(".mcp.json").is_file());
    assert!(plugin_root.join("hooks/hooks.json").is_file());
    assert!(plugin_root.join("skills/code-health/SKILL.md").is_file());
    assert!(
        plugin_root
            .join("skills/agent-managed/codex-only/SKILL.md")
            .is_file()
    );
    assert!(
        !plugin_root
            .join("skills/agent-managed/cursor-only/SKILL.md")
            .exists()
    );
    let codex_skill =
        std::fs::read_to_string(plugin_root.join("skills/agent-managed/codex-only/SKILL.md"))
            .unwrap();
    assert!(codex_skill.contains("name: codex-only"));
    assert!(codex_skill.contains(r#"description: "Use when Codex only summary""#));
    assert!(!codex_skill.contains("id: codex-only"));
    assert!(!codex_skill.contains("targets:"));
    assert!(!codex_skill.contains("checksum:"));

    let mcp = std::fs::read_to_string(plugin_root.join(".mcp.json")).unwrap();
    assert!(mcp.contains("\"command\": \"tracedecay-bin\""));
    assert!(mcp.contains("\"TRACEDECAY_ENABLE_GLOBAL_DB\": \"1\""));
}

#[tokio::test]
async fn native_overlay_sanitizes_legacy_native_frontmatter_without_blocking_peers() {
    for (legacy_id, expected_name) in [
        ("repo_hygiene", "repo-hygiene"),
        ("-repo-hygiene", "repo-hygiene"),
        ("repo-hygiene-", "repo-hygiene"),
        ("repo--hygiene", "repo-hygiene"),
        (
            "repo-hygiene-with-an-excessively-long-name-that-used-to-be-valid-for-managed-skills",
            "repo-hygiene-with-an-excessively-long-name-that-used-to-be-valid",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let profile_root = temp.path().join("profile");
        let plugin_root = temp.path().join("cursor-plugin");

        create_managed_skill(
            &profile_root,
            targeted_draft(
                "repo-hygiene",
                "Repository hygiene",
                vec![SkillInstallTarget::Cursor],
            ),
        )
        .await
        .unwrap();

        let mut legacy = targeted_draft(
            legacy_id,
            "Legacy native compatibility",
            vec![SkillInstallTarget::Cursor],
        );
        legacy.summary = "a".repeat(1020);
        create_managed_skill(&profile_root, legacy).await.unwrap();

        let summary =
            export_native_skill_overlay(&profile_root, SkillInstallTarget::Cursor, &plugin_root)
                .unwrap();
        assert_eq!(summary.exported_count, 2);
        assert!(
            plugin_root
                .join("skills/agent-managed/repo-hygiene/SKILL.md")
                .is_file()
        );

        let legacy_skill = std::fs::read_to_string(
            plugin_root
                .join("skills/agent-managed")
                .join(legacy_id)
                .join("SKILL.md"),
        )
        .unwrap();
        assert!(legacy_skill.contains(&format!("name: {expected_name}\n")));
        let description = legacy_skill
            .lines()
            .find_map(|line| line.strip_prefix("description: \""))
            .and_then(|line| line.strip_suffix('"'))
            .unwrap();
        assert_eq!(description.chars().count(), 1024);
        assert!(description.starts_with("Use when "));
    }
}

#[tokio::test]
async fn exports_only_skills_targeted_to_requested_host() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cursor_plugin = temp.path().join("cursor-plugin");
    let codex_plugin = temp.path().join("codex-plugin");
    let opencode_prompt = temp.path().join("opencode").join("AGENTS.md");

    create_managed_skill(
        &profile_root,
        targeted_draft(
            "cursor-only",
            "Cursor only",
            vec![SkillInstallTarget::Cursor],
        ),
    )
    .await
    .unwrap();
    create_managed_skill(
        &profile_root,
        targeted_draft("codex-only", "Codex only", vec![SkillInstallTarget::Codex]),
    )
    .await
    .unwrap();
    create_managed_skill(
        &profile_root,
        targeted_draft(
            "opencode-only",
            "OpenCode only",
            vec![SkillInstallTarget::OpenCode],
        ),
    )
    .await
    .unwrap();

    let cursor =
        export_native_skill_overlay(&profile_root, SkillInstallTarget::Cursor, &cursor_plugin)
            .unwrap();
    assert_eq!(cursor.exported_count, 1);
    assert_eq!(cursor.exported[0].id, "cursor-only");

    let codex =
        export_native_skill_overlay(&profile_root, SkillInstallTarget::Codex, &codex_plugin)
            .unwrap();
    assert_eq!(codex.exported_count, 1);
    assert_eq!(codex.exported[0].id, "codex-only");

    let opencode = export_prompt_skill_index(
        &profile_root,
        SkillInstallTarget::OpenCode,
        &opencode_prompt,
    )
    .unwrap();
    assert_eq!(opencode.exported_count, 1);
    assert_eq!(opencode.exported[0].id, "opencode-only");
    let prompt = std::fs::read_to_string(&opencode_prompt).unwrap();
    assert!(prompt.contains("This OpenCode index lists"));
    assert!(prompt.contains("`opencode-only`"));
    assert!(!prompt.contains("cursor-only"));
    assert!(!prompt.contains("codex-only"));
}

#[tokio::test]
async fn prompt_index_preserves_user_content_and_routes_full_body_through_mcp() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let prompt_path = temp.path().join("AGENTS.md");

    create_managed_skill(
        &profile_root,
        targeted_draft(
            "repo-hygiene",
            "Repository hygiene",
            vec![SkillInstallTarget::Agents, SkillInstallTarget::Claude],
        ),
    )
    .await
    .unwrap();
    create_managed_skill(
        &profile_root,
        targeted_draft("codex-flow", "Codex flow", vec![SkillInstallTarget::Codex]),
    )
    .await
    .unwrap();

    std::fs::write(&prompt_path, "# User rules\n\nKeep this line.\n").unwrap();
    let summary =
        export_prompt_skill_index(&profile_root, SkillInstallTarget::Agents, &prompt_path).unwrap();
    assert_eq!(summary.exported_count, 1);

    let first = std::fs::read_to_string(&prompt_path).unwrap();
    assert!(first.contains("# User rules"));
    assert!(first.contains("Keep this line."));
    assert!(first.contains("TRACEDECAY MANAGED SKILLS START"));
    assert!(first.contains("`repo-hygiene`"));
    assert!(first.contains("tracedecay_skill_view"));
    assert!(!first.contains("codex-flow"));

    let second =
        export_prompt_skill_index(&profile_root, SkillInstallTarget::Claude, &prompt_path).unwrap();
    assert_eq!(second.exported_count, 1);
    let second = std::fs::read_to_string(&prompt_path).unwrap();
    assert_eq!(second.matches("TRACEDECAY MANAGED SKILLS START").count(), 2);
    assert!(second.contains("TRACEDECAY MANAGED SKILLS START agents"));
    assert!(second.contains("TRACEDECAY MANAGED SKILLS START claude"));
    assert!(second.contains("This Claude index lists"));
}

#[tokio::test]
async fn prompt_index_repairs_slugged_orphan_end_without_claiming_user_text() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let prompt_path = temp.path().join("AGENTS.md");

    create_managed_skill(
        &profile_root,
        targeted_draft(
            "repo-hygiene",
            "Repository hygiene",
            vec![SkillInstallTarget::Agents],
        ),
    )
    .await
    .unwrap();
    std::fs::write(&prompt_path, "# User rules\n\nKeep before.\n").unwrap();
    export_prompt_skill_index(&profile_root, SkillInstallTarget::Agents, &prompt_path).unwrap();

    let stale = std::fs::read_to_string(&prompt_path)
        .unwrap()
        .replace("<!-- TRACEDECAY MANAGED SKILLS START agents -->\n", "");
    std::fs::write(&prompt_path, format!("{stale}\nKeep after.\n")).unwrap();

    export_prompt_skill_index(&profile_root, SkillInstallTarget::Agents, &prompt_path).unwrap();
    let repaired = std::fs::read_to_string(&prompt_path).unwrap();
    assert!(repaired.contains("Keep before."));
    assert!(repaired.contains("Keep after."));
    assert_eq!(
        repaired
            .matches("<!-- TRACEDECAY MANAGED SKILLS START agents -->")
            .count(),
        1
    );

    export_prompt_skill_index(&profile_root, SkillInstallTarget::Agents, &prompt_path).unwrap();
    assert_eq!(std::fs::read_to_string(&prompt_path).unwrap(), repaired);
}

#[test]
fn uninstall_repairs_legacy_orphan_end_without_claiming_user_text() {
    let temp = tempfile::tempdir().unwrap();
    let prompt_path = temp.path().join("AGENTS.md");
    let contents = concat!(
        "# User rules\n\nKeep before.\n\n",
        "## TraceDecay managed skills\n\n",
        "This AGENTS.md index lists active automatically managed skills. For full instructions, call MCP tool `tracedecay_skill_view` with the listed `id`.\n\n",
        "- `generated`: Generated.\n",
        "<!-- TRACEDECAY MANAGED SKILLS END -->\n\n",
        "Keep after.\n",
    );
    std::fs::write(&prompt_path, contents).unwrap();

    remove_prompt_skill_index_for_target(&prompt_path, SkillInstallTarget::Agents).unwrap();

    let repaired = std::fs::read_to_string(&prompt_path).unwrap();
    assert!(repaired.contains("Keep before."));
    assert!(repaired.contains("Keep after."));
    assert!(!repaired.contains("TraceDecay managed skills"));
    assert!(!repaired.contains("`generated`"));

    remove_prompt_skill_index_for_target(&prompt_path, SkillInstallTarget::Agents).unwrap();
    assert_eq!(std::fs::read_to_string(&prompt_path).unwrap(), repaired);
}

#[test]
fn prompt_index_start_only_remains_ambiguous_and_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let prompt_path = temp.path().join("AGENTS.md");
    let contents = concat!(
        "# User rules\n\n",
        "<!-- TRACEDECAY MANAGED SKILLS START agents -->\n",
        "## TraceDecay managed skills\n\n",
        "This AGENTS.md index lists active automatically managed skills. For full instructions, call MCP tool `tracedecay_skill_view` with the listed `id`.\n",
    );
    std::fs::write(&prompt_path, contents).unwrap();

    let error =
        remove_prompt_skill_index_for_target(&prompt_path, SkillInstallTarget::Agents).unwrap_err();
    assert!(error.to_string().contains("markers are unbalanced"));
    assert_eq!(std::fs::read_to_string(&prompt_path).unwrap(), contents);
}

#[test]
fn uninstall_all_removes_legacy_orphan_alongside_slugged_block() {
    let temp = tempfile::tempdir().unwrap();
    let prompt_path = temp.path().join("AGENTS.md");
    let contents = concat!(
        "# User rules\n\nKeep before.\n\n",
        "## TraceDecay managed skills\n\n",
        "This AGENTS.md index lists active automatically managed skills. For full instructions, call MCP tool `tracedecay_skill_view` with the listed `id`.\n\n",
        "- `legacy`: Legacy.\n",
        "<!-- TRACEDECAY MANAGED SKILLS END -->\n\n",
        "<!-- TRACEDECAY MANAGED SKILLS START claude -->\n",
        "## TraceDecay managed skills\n\n",
        "This Claude index lists active automatically managed skills. For full instructions, call MCP tool `tracedecay_skill_view` with the listed `id`.\n\n",
        "- `slugged`: Slugged.\n",
        "<!-- TRACEDECAY MANAGED SKILLS END claude -->\n\n",
        "Keep after.\n",
    );
    std::fs::write(&prompt_path, contents).unwrap();

    remove_prompt_skill_index(&prompt_path).unwrap();

    let repaired = std::fs::read_to_string(&prompt_path).unwrap();
    assert!(repaired.contains("Keep before."));
    assert!(repaired.contains("Keep after."));
    assert!(!repaired.contains("TraceDecay managed skills"));
    assert!(!repaired.contains("`legacy`"));
    assert!(!repaired.contains("`slugged`"));
}

#[test]
fn uninstall_all_removes_inverse_order_legacy_orphan_and_slugged_block() {
    let temp = tempfile::tempdir().unwrap();
    let prompt_path = temp.path().join("AGENTS.md");
    let contents = concat!(
        "# User rules\n\nKeep before.\n\n",
        "## TraceDecay managed skills\n\n",
        "This Claude index lists active automatically managed skills. For full instructions, call MCP tool `tracedecay_skill_view` with the listed `id`.\n\n",
        "- `legacy`: Legacy.\n",
        "<!-- TRACEDECAY MANAGED SKILLS END -->\n\n",
        "<!-- TRACEDECAY MANAGED SKILLS START agents -->\n",
        "## TraceDecay managed skills\n\n",
        "This AGENTS.md index lists active automatically managed skills. For full instructions, call MCP tool `tracedecay_skill_view` with the listed `id`.\n\n",
        "- `slugged`: Slugged.\n",
        "<!-- TRACEDECAY MANAGED SKILLS END agents -->\n\n",
        "Keep after.\n",
    );
    std::fs::write(&prompt_path, contents).unwrap();

    remove_prompt_skill_index(&prompt_path).unwrap();

    let repaired = std::fs::read_to_string(&prompt_path).unwrap();
    assert!(repaired.contains("Keep before."));
    assert!(repaired.contains("Keep after."));
    assert!(!repaired.contains("TraceDecay managed skills"));
}

#[test]
fn prompt_index_duplicate_balanced_blocks_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let prompt_path = temp.path().join("AGENTS.md");
    let block = concat!(
        "<!-- TRACEDECAY MANAGED SKILLS START agents -->\n",
        "## TraceDecay managed skills\n\n",
        "This AGENTS.md index lists active automatically managed skills. For full instructions, call MCP tool `tracedecay_skill_view` with the listed `id`.\n\n",
        "<!-- TRACEDECAY MANAGED SKILLS END agents -->\n",
    );
    let contents = format!("# User rules\n\n{block}\n{block}");
    std::fs::write(&prompt_path, &contents).unwrap();

    let error = remove_prompt_skill_index_for_target(&prompt_path, SkillInstallTarget::Agents)
        .unwrap_err()
        .to_string();

    assert!(error.contains("markers are ambiguous"));
    assert_eq!(std::fs::read_to_string(&prompt_path).unwrap(), contents);
}

#[tokio::test]
async fn prompt_index_keeps_separate_sections_for_shared_agents_md_hosts() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let agents_md = temp.path().join("AGENTS.md");

    create_managed_skill(
        &profile_root,
        targeted_draft(
            "opencode-only",
            "OpenCode only",
            vec![SkillInstallTarget::OpenCode],
        ),
    )
    .await
    .unwrap();
    create_managed_skill(
        &profile_root,
        targeted_draft("kimi-only", "Kimi only", vec![SkillInstallTarget::Kimi]),
    )
    .await
    .unwrap();

    export_prompt_skill_index(&profile_root, SkillInstallTarget::OpenCode, &agents_md).unwrap();
    export_prompt_skill_index(&profile_root, SkillInstallTarget::Kimi, &agents_md).unwrap();

    let prompt = std::fs::read_to_string(&agents_md).unwrap();
    assert!(prompt.contains("TRACEDECAY MANAGED SKILLS START opencode"));
    assert!(prompt.contains("TRACEDECAY MANAGED SKILLS START kimi"));
    assert!(prompt.contains("This OpenCode index lists"));
    assert!(prompt.contains("This Kimi index lists"));
    assert!(prompt.contains("`opencode-only`"));
    assert!(prompt.contains("`kimi-only`"));
}

#[tokio::test]
async fn uninstall_preserves_legacy_block_on_shared_file_mid_migration() {
    // A shared AGENTS.md mid-migration: host A migrated to a slugged `claude`
    // block, host B is still using the legacy unslugged block. Uninstalling a
    // third target (`agents`, which has no slugged block here) must NOT fall
    // back to deleting the legacy block, since another host's slugged block is
    // present the legacy block cannot be assumed to be ours.
    let temp = tempfile::tempdir().unwrap();
    let agents_md = temp.path().join("AGENTS.md");
    let contents = concat!(
        "# Shared prompt\n\n",
        "<!-- TRACEDECAY MANAGED SKILLS START claude -->\n",
        "Claude host index.\n",
        "<!-- TRACEDECAY MANAGED SKILLS END claude -->\n\n",
        "<!-- TRACEDECAY MANAGED SKILLS START -->\n",
        "Legacy host B index.\n",
        "<!-- TRACEDECAY MANAGED SKILLS END -->\n",
    );
    std::fs::write(&agents_md, contents).unwrap();

    remove_prompt_skill_index_for_target(&agents_md, SkillInstallTarget::Agents).unwrap();

    let after = std::fs::read_to_string(&agents_md).unwrap();
    assert!(
        after.contains("Legacy host B index."),
        "legacy block belonging to another host must be preserved: {after}"
    );
    assert!(
        after.contains("<!-- TRACEDECAY MANAGED SKILLS START -->"),
        "legacy markers must be preserved: {after}"
    );
    assert!(
        after.contains("<!-- TRACEDECAY MANAGED SKILLS START claude -->"),
        "unrelated slugged block must be preserved: {after}"
    );
}

#[tokio::test]
async fn uninstall_removes_own_slugged_block_on_shared_file() {
    // Uninstalling a target with its own slugged block removes only that block
    // and leaves the legacy block for a still-migrating host untouched.
    let temp = tempfile::tempdir().unwrap();
    let agents_md = temp.path().join("AGENTS.md");
    let contents = concat!(
        "# Shared prompt\n\n",
        "<!-- TRACEDECAY MANAGED SKILLS START claude -->\n",
        "Claude host index.\n",
        "<!-- TRACEDECAY MANAGED SKILLS END claude -->\n\n",
        "<!-- TRACEDECAY MANAGED SKILLS START -->\n",
        "Legacy host B index.\n",
        "<!-- TRACEDECAY MANAGED SKILLS END -->\n",
    );
    std::fs::write(&agents_md, contents).unwrap();

    remove_prompt_skill_index_for_target(&agents_md, SkillInstallTarget::Claude).unwrap();

    let after = std::fs::read_to_string(&agents_md).unwrap();
    assert!(
        !after.contains("Claude host index."),
        "own block removed: {after}"
    );
    assert!(
        after.contains("Legacy host B index."),
        "legacy block preserved: {after}"
    );
}

#[tokio::test]
async fn uninstall_removes_legacy_block_when_no_slugged_blocks_remain() {
    // When only a legacy unslugged block exists (no other host has migrated),
    // per-target uninstall may safely reclaim it.
    let temp = tempfile::tempdir().unwrap();
    let agents_md = temp.path().join("AGENTS.md");
    let contents = concat!(
        "# Shared prompt\n\n",
        "<!-- TRACEDECAY MANAGED SKILLS START -->\n",
        "Legacy index.\n",
        "<!-- TRACEDECAY MANAGED SKILLS END -->\n",
    );
    std::fs::write(&agents_md, contents).unwrap();

    remove_prompt_skill_index_for_target(&agents_md, SkillInstallTarget::Agents).unwrap();

    let after = std::fs::read_to_string(&agents_md).unwrap();
    assert!(
        !after.contains("Legacy index."),
        "legacy block removed: {after}"
    );
}

#[tokio::test]
async fn native_overlay_keeps_previous_export_when_rebuild_fails() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let plugin_root = temp.path().join("cursor-plugin");

    create_managed_skill(
        &profile_root,
        targeted_draft(
            "repo-hygiene",
            "Repository hygiene",
            vec![SkillInstallTarget::Claude, SkillInstallTarget::Cursor],
        ),
    )
    .await
    .unwrap();
    export_native_skill_overlay(&profile_root, SkillInstallTarget::Cursor, &plugin_root).unwrap();

    let previous_skill = plugin_root.join("skills/agent-managed/repo-hygiene/SKILL.md");
    assert!(previous_skill.is_file());

    let mut corrupted = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    corrupted.support_files.push(ManagedSupportFile {
        path: std::path::PathBuf::from("../escape.md"),
        bytes: b"escape".to_vec(),
    });
    let skill_dir = managed_skill_dir(&profile_root, "repo-hygiene").unwrap();
    std::fs::write(
        skill_dir.join("skill.json"),
        serde_json::to_vec_pretty(&corrupted).unwrap(),
    )
    .unwrap();

    let err = export_native_skill_overlay(&profile_root, SkillInstallTarget::Cursor, &plugin_root)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unsafe support path"));
    assert!(
        previous_skill.is_file(),
        "failed rebuild must preserve the last complete overlay"
    );
}

/// Fakes a Claude Code global install under `home`: the tracedecay MCP
/// registration in `.claude.json` plus the CLAUDE.md prompt file the export
/// writes its skill index into.
fn install_fake_claude(home: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let claude_md = home.join(".claude/CLAUDE.md");
    std::fs::write(&claude_md, "# Claude rules\n").unwrap();
    std::fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers":{"tracedecay":{"command":"tracedecay","args":["serve"]}}}"#,
    )
    .unwrap();
    claude_md
}

/// Fakes an installed Cursor plugin bundle under `home` (manifest presence is
/// the detection signal) and returns the plugin install dir.
fn install_fake_cursor_plugin(home: &std::path::Path) -> std::path::PathBuf {
    let plugin_dir = home.join(".cursor/plugins/local/tracedecay");
    std::fs::create_dir_all(plugin_dir.join(".cursor-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".cursor-plugin/plugin.json"),
        r#"{"name":"tracedecay"}"#,
    )
    .unwrap();
    plugin_dir
}

#[tokio::test]
async fn lifecycle_export_sweep_deploys_and_retracts_across_detected_agents() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let profile_root = home.join(".tracedecay");
    let claude_md = install_fake_claude(&home);
    let cursor_plugin = install_fake_cursor_plugin(&home);

    create_managed_skill(
        &profile_root,
        targeted_draft(
            "repo-hygiene",
            "Repository hygiene",
            vec![SkillInstallTarget::Claude, SkillInstallTarget::Cursor],
        ),
    )
    .await
    .unwrap();

    let reports = export_managed_skills_to_agents(&home, &profile_root);
    let agents: Vec<&str> = reports.iter().map(|report| report.agent.as_str()).collect();
    assert_eq!(agents, vec!["claude", "cursor"], "reports: {reports:?}");
    for report in &reports {
        assert_eq!(report.error, None, "{} export failed", report.agent);
        assert_eq!(report.exports.len(), 1);
        assert_eq!(report.exports[0].exported_count, 1);
        assert_eq!(report.exports[0].exported[0].id, "repo-hygiene");
    }
    assert!(
        cursor_plugin
            .join("skills/agent-managed/repo-hygiene/SKILL.md")
            .is_file()
    );
    let claude_contents = std::fs::read_to_string(&claude_md).unwrap();
    assert!(claude_contents.contains("`repo-hygiene`"));
    assert!(claude_contents.contains("# Claude rules"));

    disable_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let reports = export_managed_skills_to_agents(&home, &profile_root);
    for report in &reports {
        assert_eq!(report.error, None, "{} retraction failed", report.agent);
        assert_eq!(report.exports[0].exported_count, 0);
    }
    assert!(
        !cursor_plugin
            .join("skills/agent-managed/repo-hygiene/SKILL.md")
            .exists()
    );
    let claude_contents = std::fs::read_to_string(&claude_md).unwrap();
    assert!(!claude_contents.contains("repo-hygiene"));
    assert!(claude_contents.contains("# Claude rules"));
}

#[tokio::test]
async fn lifecycle_export_sweep_isolates_per_agent_failures() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let profile_root = home.join(".tracedecay");
    let claude_md = install_fake_claude(&home);
    let cursor_plugin = install_fake_cursor_plugin(&home);
    // A directory where the prompt file should be makes the Claude export
    // fail while leaving the Cursor overlay export unaffected.
    std::fs::remove_file(&claude_md).unwrap();
    std::fs::create_dir_all(&claude_md).unwrap();

    create_managed_skill(
        &profile_root,
        targeted_draft(
            "repo-hygiene",
            "Repository hygiene",
            vec![SkillInstallTarget::Claude, SkillInstallTarget::Cursor],
        ),
    )
    .await
    .unwrap();

    let reports = export_managed_skills_to_agents(&home, &profile_root);
    let claude = reports
        .iter()
        .find(|report| report.agent == "claude")
        .expect("claude failure must be reported");
    assert!(claude.error.is_some());
    assert!(claude.exports.is_empty());
    let cursor = reports
        .iter()
        .find(|report| report.agent == "cursor")
        .expect("cursor export must still run");
    assert_eq!(cursor.error, None);
    assert_eq!(cursor.exports[0].exported_count, 1);
    assert!(
        cursor_plugin
            .join("skills/agent-managed/repo-hygiene/SKILL.md")
            .is_file()
    );
}

#[tokio::test]
async fn lifecycle_export_sweep_skips_agents_without_installs() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let profile_root = home.join(".tracedecay");
    std::fs::create_dir_all(&home).unwrap();

    create_managed_skill(&profile_root, draft("repo-hygiene", "Repository hygiene"))
        .await
        .unwrap();

    let reports = export_managed_skills_to_agents(&home, &profile_root);
    assert!(
        reports.is_empty(),
        "no detected installs means no export destinations: {reports:?}"
    );
}

#[tokio::test]
async fn lifecycle_export_sweep_skips_non_default_profiles() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let profile_root = temp.path().join("test-profile/.tracedecay");
    let claude_md = install_fake_claude(&home);
    let original = std::fs::read_to_string(&claude_md).unwrap();

    create_managed_skill(&profile_root, draft("repo-hygiene", "Repository hygiene"))
        .await
        .unwrap();

    assert!(export_managed_skills_to_agents(&home, &profile_root).is_empty());
    assert_eq!(std::fs::read_to_string(claude_md).unwrap(), original);
}

#[tokio::test]
async fn local_lifecycle_export_skips_unrelated_project_configs() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let project_root = temp.path().join("project");
    let profile_root = home.join(".tracedecay");
    std::fs::create_dir_all(&home).unwrap();

    let claude_md = project_root.join(".claude/CLAUDE.md");
    std::fs::create_dir_all(claude_md.parent().unwrap()).unwrap();
    std::fs::write(&claude_md, "# Claude rules\n").unwrap();
    std::fs::write(
        project_root.join(".mcp.json"),
        r#"{"mcpServers":{"other":{"command":"other"}}}"#,
    )
    .unwrap();

    let copilot_md = project_root.join(".github/copilot-instructions.md");
    std::fs::create_dir_all(copilot_md.parent().unwrap()).unwrap();
    std::fs::write(&copilot_md, "# Copilot rules\n").unwrap();
    std::fs::create_dir_all(project_root.join(".vscode")).unwrap();
    std::fs::write(
        project_root.join(".vscode/mcp.json"),
        r#"{"servers":{"other":{"command":"other"}}}"#,
    )
    .unwrap();

    let agents_md = project_root.join("AGENTS.md");
    std::fs::write(&agents_md, "# Agent rules\n").unwrap();
    std::fs::create_dir_all(project_root.join(".kimi-code")).unwrap();
    std::fs::write(
        project_root.join(".kimi-code/mcp.json"),
        r#"{"mcpServers":{"other":{"command":"other"}}}"#,
    )
    .unwrap();
    std::fs::write(
        project_root.join("opencode.json"),
        r#"{"mcp":{"other":{"command":["other"]}}}"#,
    )
    .unwrap();

    let vibe_prompt = project_root.join(".vibe/prompts/cli.md");
    std::fs::create_dir_all(vibe_prompt.parent().unwrap()).unwrap();
    std::fs::write(&vibe_prompt, "# Vibe rules\n").unwrap();
    std::fs::write(
        project_root.join(".vibe/config.toml"),
        r#"[[mcp_servers]]
name = "other"
command = "other"
"#,
    )
    .unwrap();

    let kiro_index = project_root.join(".kiro/steering/tracedecay-managed-skills.md");
    std::fs::create_dir_all(kiro_index.parent().unwrap()).unwrap();
    std::fs::write(&kiro_index, "# Kiro managed skills\n").unwrap();
    std::fs::create_dir_all(project_root.join(".kiro/settings")).unwrap();
    std::fs::write(
        project_root.join(".kiro/settings/mcp.json"),
        r#"{"mcpServers":{"other":{"command":"other"}}}"#,
    )
    .unwrap();

    create_managed_skill(&profile_root, draft("repo-hygiene", "Repository hygiene"))
        .await
        .unwrap();

    let reports = export_managed_skills_to_agent_hosts(&home, &project_root, &profile_root);
    assert!(
        reports.is_empty(),
        "unrelated project configs must not become export destinations: {reports:?}"
    );
    for path in [
        &claude_md,
        &copilot_md,
        &agents_md,
        &vibe_prompt,
        &kiro_index,
    ] {
        let contents = std::fs::read_to_string(path).unwrap();
        assert!(
            !contents.contains("repo-hygiene"),
            "unrelated config refreshed {}: {contents}",
            path.display()
        );
    }
}

#[tokio::test]
async fn hermes_target_uses_native_plugin_overlay() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let prompt_path = temp.path().join("HERMES.md");
    let plugin_root = temp.path().join("plugin");

    create_managed_skill(&profile_root, draft("repo-hygiene", "Repository hygiene"))
        .await
        .unwrap();

    let err = export_prompt_skill_index(&profile_root, SkillInstallTarget::Hermes, &prompt_path)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Hermes owns profile skills"));
    assert!(!prompt_path.exists());

    let summary =
        install_managed_skills(&profile_root, SkillInstallTarget::Hermes, &plugin_root).unwrap();
    assert_eq!(summary.exported_count, 1);
    assert_eq!(summary.exported[0].id, "repo-hygiene");
    assert!(
        plugin_root
            .join("skills/agent-managed/repo-hygiene/SKILL.md")
            .is_file()
    );
}

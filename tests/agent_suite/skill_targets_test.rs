use serde_json::json;

use tracedecay::agents::codex::export_codex_plugin_artifact;
use tracedecay::automation::hermes_bridge::{load_hermes_skill_bridge, HermesSkillBridgeOptions};
use tracedecay::automation::managed_skills::{
    approve_managed_skill, create_managed_skill_draft, default_managed_skill_targets,
    disable_managed_skill, ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource,
    ManagedSupportFile,
};
use tracedecay::automation::skill_targets::{
    export_native_skill_overlay, export_prompt_skill_index, install_managed_skills,
    SkillInstallTarget,
};

fn draft(id: &str, title: &str) -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: id.to_string(),
        title: title.to_string(),
        summary: format!("{title} summary"),
        category: "workflow".to_string(),
        targets: default_managed_skill_targets(),
        body_markdown: format!("Use {title} when the workflow repeats."),
        support_files: vec![ManagedSupportFile::new(
            "references/checklist.md",
            format!("- {id}\n").into_bytes(),
        )
        .unwrap()],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::UserDraft,
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

#[tokio::test]
async fn native_overlay_exports_only_active_skills_and_prunes_generated_namespace() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let plugin_root = temp.path().join("cursor-plugin");

    create_managed_skill_draft(&profile_root, draft("repo-hygiene", "Repository hygiene"))
        .await
        .unwrap();
    approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    create_managed_skill_draft(&profile_root, draft("pending-flow", "Pending flow"))
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
    assert!(plugin_root
        .join("skills/agent-managed/repo-hygiene/SKILL.md")
        .is_file());
    assert!(plugin_root
        .join("skills/agent-managed/repo-hygiene/references/checklist.md")
        .is_file());
    assert!(!plugin_root
        .join("skills/agent-managed/pending-flow/SKILL.md")
        .exists());
    assert!(!plugin_root
        .join("skills/agent-managed/stale-skill/SKILL.md")
        .exists());
    assert!(plugin_root.join("skills/static-skill/SKILL.md").is_file());
    assert!(plugin_root
        .join("skills/agent-managed/.tracedecay-managed-skills.json")
        .is_file());

    disable_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let summary =
        export_native_skill_overlay(&profile_root, SkillInstallTarget::Cursor, &plugin_root)
            .unwrap();
    assert_eq!(summary.exported_count, 0);
    assert!(!plugin_root
        .join("skills/agent-managed/repo-hygiene/SKILL.md")
        .exists());
    assert!(plugin_root.join("skills/static-skill/SKILL.md").is_file());
}

#[tokio::test]
async fn codex_native_overlay_uses_agent_managed_namespace() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let plugin_root = temp.path().join("codex-plugin");

    create_managed_skill_draft(&profile_root, draft("repo-hygiene", "Repository hygiene"))
        .await
        .unwrap();
    approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();

    let summary =
        export_native_skill_overlay(&profile_root, SkillInstallTarget::Codex, &plugin_root)
            .unwrap();
    assert_eq!(summary.exported_count, 1);
    assert!(plugin_root
        .join("skills/agent-managed/repo-hygiene/SKILL.md")
        .is_file());
}

#[tokio::test]
async fn codex_plugin_artifact_exports_shareable_bundle_with_managed_skills() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let plugin_root = temp.path().join("codex-plugin");

    create_managed_skill_draft(
        &profile_root,
        targeted_draft("codex-only", "Codex only", vec![SkillInstallTarget::Codex]),
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "codex-only")
        .await
        .unwrap();
    create_managed_skill_draft(
        &profile_root,
        targeted_draft(
            "cursor-only",
            "Cursor only",
            vec![SkillInstallTarget::Cursor],
        ),
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "cursor-only")
        .await
        .unwrap();

    let summary = export_codex_plugin_artifact(&profile_root, &plugin_root, "tracedecay-bin")
        .expect("Codex plugin artifact should export");

    assert_eq!(summary.exported_count, 1);
    assert_eq!(summary.exported[0].id, "codex-only");
    assert!(plugin_root.join(".codex-plugin/plugin.json").is_file());
    assert!(plugin_root.join(".mcp.json").is_file());
    assert!(plugin_root.join("hooks/hooks.json").is_file());
    assert!(plugin_root
        .join("skills/architecture-overview/SKILL.md")
        .is_file());
    assert!(plugin_root
        .join("skills/agent-managed/codex-only/SKILL.md")
        .is_file());
    assert!(!plugin_root
        .join("skills/agent-managed/cursor-only/SKILL.md")
        .exists());

    let mcp = std::fs::read_to_string(plugin_root.join(".mcp.json")).unwrap();
    assert!(mcp.contains("\"command\": \"tracedecay-bin\""));
    assert!(mcp.contains("\"TRACEDECAY_ENABLE_GLOBAL_DB\": \"1\""));
}

#[tokio::test]
async fn exports_only_skills_targeted_to_requested_host() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cursor_plugin = temp.path().join("cursor-plugin");
    let codex_plugin = temp.path().join("codex-plugin");
    let opencode_prompt = temp.path().join("opencode").join("AGENTS.md");

    create_managed_skill_draft(
        &profile_root,
        targeted_draft(
            "cursor-only",
            "Cursor only",
            vec![SkillInstallTarget::Cursor],
        ),
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "cursor-only")
        .await
        .unwrap();
    create_managed_skill_draft(
        &profile_root,
        targeted_draft("codex-only", "Codex only", vec![SkillInstallTarget::Codex]),
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "codex-only")
        .await
        .unwrap();
    create_managed_skill_draft(
        &profile_root,
        targeted_draft(
            "opencode-only",
            "OpenCode only",
            vec![SkillInstallTarget::OpenCode],
        ),
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "opencode-only")
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

    create_managed_skill_draft(&profile_root, draft("repo-hygiene", "Repository hygiene"))
        .await
        .unwrap();
    approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    create_managed_skill_draft(&profile_root, draft("pending-flow", "Pending flow"))
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
    assert!(!first.contains("pending-flow"));

    let second =
        export_prompt_skill_index(&profile_root, SkillInstallTarget::Claude, &prompt_path).unwrap();
    assert_eq!(second.exported_count, 1);
    let second = std::fs::read_to_string(&prompt_path).unwrap();
    assert_eq!(second.matches("TRACEDECAY MANAGED SKILLS START").count(), 1);
    assert!(second.contains("This Claude index lists"));
}

#[tokio::test]
async fn hermes_target_is_host_owned_and_not_exported_by_tracedecay() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let prompt_path = temp.path().join("HERMES.md");

    create_managed_skill_draft(&profile_root, draft("repo-hygiene", "Repository hygiene"))
        .await
        .unwrap();
    approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();

    let err = export_prompt_skill_index(&profile_root, SkillInstallTarget::Hermes, &prompt_path)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Hermes owns profile skills"));
    assert!(!prompt_path.exists());

    let err = install_managed_skills(&profile_root, SkillInstallTarget::Hermes, &prompt_path)
        .unwrap_err()
        .to_string();
    assert!(err.contains("read-only Hermes skill bridge"));
}

#[test]
fn hermes_skill_bridge_reads_profile_owned_skills_pending_and_usage() {
    let temp = tempfile::tempdir().unwrap();
    let hermes_home = temp.path().join("hermes");
    let skills_dir = hermes_home.join("skills");
    let skill_dir = skills_dir.join("workflow").join("repo-hygiene");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: repo-hygiene\ndescription: Keep repo work clean\n---\n\nUse focused tests.\n",
    )
    .unwrap();
    std::fs::write(
        skills_dir.join(".usage.json"),
        r#"{"repo-hygiene":{"created_by":"agent","use_count":2,"pinned":true}}"#,
    )
    .unwrap();
    let pending_dir = hermes_home.join("pending").join("skills");
    std::fs::create_dir_all(&pending_dir).unwrap();
    std::fs::write(
        pending_dir.join("abc123.json"),
        r#"{"id":"abc123","subsystem":"skills","action":"patch","summary":"tighten repo hygiene","origin":"background_review","created_at":123,"payload":{"action":"patch","name":"repo-hygiene","old_string":"Use","new_string":"Prefer"}}"#,
    )
    .unwrap();
    std::fs::write(
        pending_dir.join("newer.json"),
        r#"{"id":"newer","subsystem":"skills","action":"patch","summary":"newer repo hygiene","origin":"background_review","created_at":200,"payload":{"action":"patch","name":"repo-hygiene","old_string":"tests","new_string":"checks"}}"#,
    )
    .unwrap();
    std::fs::write(pending_dir.join("bad.json"), "{not json").unwrap();
    std::fs::create_dir_all(skills_dir.join(".archive").join("old-skill")).unwrap();
    std::fs::write(
        hermes_home.join("config.json"),
        r#"{"project_root":"/work/repo","memory":{"write_approval":true},"skills":{"write_approval":"json-pending"}}"#,
    )
    .unwrap();
    std::fs::write(
        hermes_home.join("config.yaml"),
        r#"
terminal:
  cwd: /work/yaml-repo
plugins:
  tracedecay:
    project_root: /work/repo-from-yaml
curator:
  enabled: true
  interval_hours: 24
  min_idle_hours: 2
  stale_after_days: 30
  archive_after_days: 90
  auxiliary:
    provider: openai
    model: gpt-test
    base_url: "https://example.invalid/v1#curator"
    api_key: secret-value
memory:
  nudge_interval: 12
  write_approval: manual
skills:
  creation_nudge_interval: 15
  write_approval: pending
"#,
    )
    .unwrap();
    std::fs::write(
        skills_dir.join(".curator_state"),
        r#"{"paused":true,"last_run_at":"2026-06-24T00:00:00Z","last_run_summary":"archived stale skills","last_report_path":"logs/curator/report.json","run_count":3}"#,
    )
    .unwrap();
    std::fs::write(hermes_home.join("state.db"), b"").unwrap();

    let snapshot = load_hermes_skill_bridge(
        &hermes_home,
        HermesSkillBridgeOptions {
            include_skill_bodies: true,
            include_pending_payloads: false,
        },
    )
    .unwrap();

    assert_eq!(snapshot.contracts.lifecycle_owner, "hermes");
    assert!(snapshot.config.exists);
    assert!(snapshot.config.config_yaml_exists);
    assert_eq!(snapshot.config.config_format, "yaml");
    assert_eq!(
        snapshot.config.project_root_pin.as_ref(),
        Some(&json!("/work/repo-from-yaml"))
    );
    assert_eq!(snapshot.config.curator.enabled, Some(true));
    assert_eq!(snapshot.config.curator.interval_hours, Some(24));
    assert_eq!(snapshot.config.self_improvement.memory_nudge_interval, 12);
    assert_eq!(
        snapshot
            .config
            .self_improvement
            .skill_creation_nudge_interval,
        15
    );
    assert_eq!(snapshot.config.write_approval.memory, Some(json!("manual")));
    assert!(!snapshot.config.write_approval.memory_enabled);
    assert_eq!(
        snapshot.config.write_approval.skills,
        Some(json!("pending"))
    );
    assert!(!snapshot.config.write_approval.skills_enabled);
    let auxiliary_curator = snapshot.config.auxiliary_curator.as_ref().unwrap();
    assert_eq!(auxiliary_curator.provider.as_deref(), Some("openai"));
    assert_eq!(auxiliary_curator.model.as_deref(), Some("gpt-test"));
    assert_eq!(
        auxiliary_curator.base_url.as_deref(),
        Some("https://example.invalid/v1#curator")
    );
    assert!(auxiliary_curator.api_key_configured);
    let snapshot_json = serde_json::to_string(&snapshot).unwrap();
    assert!(!snapshot_json.contains("secret-value"));
    assert!(snapshot.state.exists);
    assert_eq!(snapshot.state.projection_policy, "session_messages_only");
    assert_eq!(snapshot.state.state_db_path, hermes_home.join("state.db"));
    assert_eq!(
        snapshot.state.hermes_state_db_path,
        hermes_home.join("state.db")
    );
    assert_eq!(
        snapshot.state.profile_lcm_db_path,
        hermes_home.join(".tracedecay").join("sessions.db")
    );
    assert_eq!(
        snapshot.state.trace_decay_lcm_store_path,
        hermes_home.join(".tracedecay").join("sessions.db")
    );
    assert_eq!(
        snapshot.state.state_db_projection_policy,
        "read_only_session_message_projection"
    );
    assert_eq!(snapshot.state.raw_lcm_owner, "hermes_runtime");
    assert_eq!(snapshot.state.hermes_state_owner, "hermes_runtime");
    assert_eq!(snapshot.state.session_db_owner, "hermes_runtime");
    assert_eq!(
        snapshot.state.profile_lcm_store_owner,
        "tracedecay_hermes_plugin"
    );
    assert_eq!(
        snapshot.state.trace_decay_lcm_store_owner,
        "tracedecay_hermes_plugin"
    );
    assert_eq!(
        snapshot.state.trace_decay_lcm_role,
        "hermes_profile_session_store"
    );
    assert_eq!(
        snapshot.state.trace_decay_ingest_role,
        "read_only_session_message_projector"
    );
    assert_eq!(
        snapshot.state.projected_tables,
        vec!["sessions".to_string(), "session_messages".to_string()]
    );
    assert_eq!(snapshot.curator.owner, "hermes");
    assert_eq!(snapshot.curator.trace_decay_role, "read_only_projector");
    assert!(snapshot.curator.standalone_automation_blocked);
    assert!(snapshot.curator.state.exists);
    assert_eq!(snapshot.curator.state.paused, Some(true));
    assert_eq!(snapshot.curator.state.run_count, Some(3));
    assert_eq!(
        snapshot.curator.policy.eligible_provenance,
        vec!["agent".to_string(), "agent_created".to_string()]
    );
    assert_eq!(snapshot.curator.policy.max_destructive_action, "archive");
    assert!(snapshot.curator.policy.pinned_exempt);
    assert_eq!(snapshot.background_review.owner, "hermes_runtime");
    assert_eq!(snapshot.background_review.origin, "background_review");
    assert_eq!(snapshot.background_review.memory_nudge_interval, 12);
    assert_eq!(snapshot.background_review.skill_nudge_interval, 15);
    assert!(!snapshot.background_review.runtime_counters_projected);
    assert_eq!(snapshot.skill_count, 1);
    assert_eq!(snapshot.pending_skill_count, 2);
    assert_eq!(snapshot.usage_record_count, 1);
    assert_eq!(snapshot.archive_count, 1);
    assert_eq!(snapshot.skills[0].name, "repo-hygiene");
    assert_eq!(snapshot.skills[0].ownership.owner, "hermes_local");
    assert!(snapshot.skills[0].ownership.curator_managed_record);
    assert!(snapshot.skills[0].ownership.curator_eligible);
    assert_eq!(snapshot.skills[0].category.as_deref(), Some("workflow"));
    assert_eq!(
        snapshot.skills[0].description.as_deref(),
        Some("Keep repo work clean")
    );
    assert_eq!(
        snapshot.skills[0].pending_write_ids,
        vec!["abc123".to_string(), "newer".to_string()]
    );
    assert_eq!(
        snapshot.skills[0].usage.as_ref().unwrap()["created_by"],
        "agent"
    );
    assert!(snapshot.skills[0]
        .body_markdown
        .as_ref()
        .unwrap()
        .contains("Use focused tests"));
    assert_eq!(
        snapshot.pending_skills[0].origin.as_deref(),
        Some("background_review")
    );
    assert_eq!(
        snapshot.pending_skills[0].subsystem.as_deref(),
        Some("skills")
    );
    assert_eq!(
        snapshot.pending_skills[0].source_path,
        pending_dir.join("abc123.json")
    );
    assert!(snapshot.pending_skills[0].payload.is_none());
    assert_eq!(
        snapshot.pending_skills[1].source_path,
        pending_dir.join("newer.json")
    );
    assert!(!snapshot
        .pending_skills
        .iter()
        .any(|pending| pending.id == "bad"));
}

#[test]
fn hermes_skill_bridge_sorts_pending_created_at_float_values() {
    let temp = tempfile::tempdir().unwrap();
    let hermes_home = temp.path().join("hermes");
    let skills_dir = hermes_home.join("skills");
    let skill_dir = skills_dir.join("repo-hygiene");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: repo-hygiene\n---\n\nUse focused tests.\n",
    )
    .unwrap();
    let pending_dir = hermes_home.join("pending").join("skills");
    std::fs::create_dir_all(&pending_dir).unwrap();
    std::fs::write(
        pending_dir.join("z-later.json"),
        r#"{"id":"z-later","created_at":200.1,"payload":{"name":"repo-hygiene"}}"#,
    )
    .unwrap();
    std::fs::write(
        pending_dir.join("a-earlier.json"),
        r#"{"id":"a-earlier","created_at":123.4,"payload":{"name":"repo-hygiene"}}"#,
    )
    .unwrap();
    std::fs::write(
        pending_dir.join("m-integer.json"),
        r#"{"id":"m-integer","created_at":124,"payload":{"name":"repo-hygiene"}}"#,
    )
    .unwrap();
    std::fs::write(
        pending_dir.join("b-string.json"),
        r#"{"id":"b-string","created_at":"2026-06-24T00:00:00Z","payload":{"name":"repo-hygiene"}}"#,
    )
    .unwrap();
    std::fs::write(
        pending_dir.join("a-missing.json"),
        r#"{"id":"a-missing","payload":{"name":"repo-hygiene"}}"#,
    )
    .unwrap();

    let snapshot =
        load_hermes_skill_bridge(&hermes_home, HermesSkillBridgeOptions::default()).unwrap();

    let ids = snapshot
        .pending_skills
        .iter()
        .map(|pending| pending.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec!["a-earlier", "m-integer", "z-later", "b-string", "a-missing"]
    );
    assert_eq!(snapshot.pending_skills[0].created_at, Some(json!(123.4)));
    assert_eq!(
        snapshot.skills[0].pending_write_ids,
        vec![
            "a-earlier".to_string(),
            "m-integer".to_string(),
            "z-later".to_string(),
            "b-string".to_string(),
            "a-missing".to_string()
        ]
    );
}

#[test]
fn hermes_skill_bridge_projects_effective_write_approval_booleans() {
    let temp = tempfile::tempdir().unwrap();
    let hermes_home = temp.path().join("hermes");
    std::fs::create_dir_all(&hermes_home).unwrap();
    std::fs::write(
        hermes_home.join("config.json"),
        r#"{"memory":{"write_approval":"approve"},"skills":{"write_approval":false}}"#,
    )
    .unwrap();

    let snapshot =
        load_hermes_skill_bridge(&hermes_home, HermesSkillBridgeOptions::default()).unwrap();
    assert_eq!(
        snapshot.config.write_approval.memory,
        Some(json!("approve"))
    );
    assert!(snapshot.config.write_approval.memory_enabled);
    assert_eq!(snapshot.config.write_approval.skills, Some(json!(false)));
    assert!(!snapshot.config.write_approval.skills_enabled);

    std::fs::write(
        hermes_home.join("config.yaml"),
        r#"
memory:
  write_approval: off
skills:
  write_approval: enabled
"#,
    )
    .unwrap();

    let snapshot =
        load_hermes_skill_bridge(&hermes_home, HermesSkillBridgeOptions::default()).unwrap();
    assert_eq!(snapshot.config.write_approval.memory, Some(json!(false)));
    assert!(!snapshot.config.write_approval.memory_enabled);
    assert_eq!(
        snapshot.config.write_approval.skills,
        Some(json!("enabled"))
    );
    assert!(snapshot.config.write_approval.skills_enabled);
}

#[test]
fn hermes_skill_bridge_projects_skill_ownership_markers() {
    let temp = tempfile::tempdir().unwrap();
    let hermes_home = temp.path().join("hermes");
    let skills_dir = hermes_home.join("skills");
    for name in [
        "bundled-skill",
        "hub-skill",
        "local-agent",
        "plan",
        "suppressed-skill",
    ] {
        let skill_dir = skills_dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\n---\n\nUse {name}.\n"),
        )
        .unwrap();
    }
    std::fs::write(
        skills_dir.join(".usage.json"),
        r#"{"local-agent":{"created_by":"agent"},"hub-skill":{"created_by":"agent"}}"#,
    )
    .unwrap();
    std::fs::write(
        skills_dir.join(".bundled_manifest"),
        "bundled-skill:abc\nplan:def\nsuppressed-skill:ghi\n",
    )
    .unwrap();
    std::fs::write(skills_dir.join(".curator_suppressed"), "suppressed-skill\n").unwrap();
    std::fs::create_dir_all(skills_dir.join(".hub")).unwrap();
    std::fs::write(
        skills_dir.join(".hub").join("lock.json"),
        r#"{"installed":{"hub-skill":{"install_path":"hub-skill"}}}"#,
    )
    .unwrap();

    let snapshot =
        load_hermes_skill_bridge(&hermes_home, HermesSkillBridgeOptions::default()).unwrap();
    let find = |name: &str| {
        snapshot
            .skills
            .iter()
            .find(|skill| skill.name == name)
            .unwrap()
    };

    let bundled = &find("bundled-skill").ownership;
    assert_eq!(bundled.owner, "hermes_bundle");
    assert!(bundled.bundled);
    assert!(bundled.curator_eligible);

    let hub = &find("hub-skill").ownership;
    assert_eq!(hub.owner, "hermes_hub");
    assert!(hub.hub_installed);
    assert!(!hub.curator_eligible);

    let local = &find("local-agent").ownership;
    assert_eq!(local.owner, "hermes_local");
    assert!(local.curator_managed_record);
    assert!(local.curator_eligible);

    let plan = &find("plan").ownership;
    assert!(plan.protected_builtin);
    assert!(!plan.curator_eligible);

    let suppressed = &find("suppressed-skill").ownership;
    assert!(suppressed.curator_suppressed);
    assert!(!suppressed.curator_eligible);
}

#[test]
fn hermes_skill_bridge_treats_corrupt_usage_sidecar_as_empty() {
    let temp = tempfile::tempdir().unwrap();
    let hermes_home = temp.path().join("hermes");
    let skills_dir = hermes_home.join("skills");
    let skill_dir = skills_dir.join("repo-hygiene");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: repo-hygiene\ndescription: Keep repo work clean\n---\n\nUse focused tests.\n",
    )
    .unwrap();
    std::fs::write(skills_dir.join(".usage.json"), "{not json").unwrap();

    let snapshot =
        load_hermes_skill_bridge(&hermes_home, HermesSkillBridgeOptions::default()).unwrap();

    assert_eq!(snapshot.skill_count, 1);
    assert_eq!(snapshot.usage_record_count, 0);
    assert!(snapshot.usage_records.is_empty());
    assert!(snapshot.skills[0].usage.is_none());
}

use tracedecay::automation::managed_skills::{
    approve_managed_skill, archive_managed_skill, create_managed_skill_draft,
    disable_managed_skill, discard_pending_managed_skill_update, list_managed_skills,
    load_managed_skill, managed_skill_dir, restore_managed_skill, save_managed_skill,
    set_managed_skill_state, stage_managed_skill_update, update_managed_skill, ManagedSkillDraft,
    ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState, ManagedSkillUpdate,
    ManagedSupportFile, SkillInstallTarget, MAX_MANAGED_SKILL_BODY_BYTES,
};
use tracedecay::automation::skill_usage::{
    ingest_analytics_events, load_skill_usage_records, record_skill_usage,
    record_skill_usage_event, skill_usage_ledger_path, summarize_skill_usage,
    summarize_skill_usage_for, SkillUsageAction, SkillUsageEvent,
};
use tracedecay::global_db::AnalyticsEventRecord;

fn draft() -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: "repo-hygiene".to_string(),
        title: "Repository hygiene".to_string(),
        summary: "Keep repository maintenance guidance current.".to_string(),
        category: "maintenance".to_string(),
        targets: vec![SkillInstallTarget::Cursor, SkillInstallTarget::Codex],
        body_markdown: "Use focused checks before changing generated files.".to_string(),
        support_files: vec![ManagedSupportFile::new(
            "references/checklist.md",
            b"- check dirty tree\n- run focused tests\n".to_vec(),
        )
        .unwrap()],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "tracedecay".to_string(),
            run_id: Some("run_123".to_string()),
        },
    }
}

#[test]
fn rejects_unsafe_skill_ids_and_support_paths() {
    for id in [
        "",
        "../escape",
        "bad/name",
        ".hidden",
        "Bad Name",
        "repo..x",
    ] {
        let mut draft = draft();
        draft.id = id.to_string();
        assert!(draft.materialize().is_err(), "accepted unsafe id: {id}");
    }

    for path in [
        "",
        "/tmp/escape.md",
        "../escape.md",
        "a/../../b.md",
        "a\\b.md",
        "SKILL.md",
        "skill.json",
        "notes/freeform.md",
        "references",
    ] {
        assert!(
            ManagedSupportFile::new(path, b"body".to_vec()).is_err(),
            "accepted unsafe support path: {path}",
        );
    }
}

#[test]
fn rejects_duplicate_conflicting_and_oversized_support_files() {
    let mut duplicate = draft();
    duplicate.support_files = vec![
        ManagedSupportFile::new("references/checklist.md", b"one".to_vec()).unwrap(),
        ManagedSupportFile::new("references/checklist.md", b"two".to_vec()).unwrap(),
    ];
    assert!(duplicate.materialize().is_err());

    let mut conflict = draft();
    conflict.support_files = vec![
        ManagedSupportFile::new("references/checklist.md", b"one".to_vec()).unwrap(),
        ManagedSupportFile::new("references/checklist.md/detail.md", b"two".to_vec()).unwrap(),
    ];
    assert!(conflict.materialize().is_err());

    assert!(ManagedSupportFile::new("references/huge.md", vec![b'x'; 64 * 1024 + 1]).is_err());
}

#[test]
fn validates_minimum_metadata_and_renders_frontmatter() {
    for (field, value) in [
        ("title", ""),
        ("summary", ""),
        ("category", ""),
        ("body_markdown", ""),
    ] {
        let mut draft = draft();
        match field {
            "title" => draft.title = value.to_string(),
            "summary" => draft.summary = value.to_string(),
            "category" => draft.category = value.to_string(),
            "body_markdown" => draft.body_markdown = value.to_string(),
            _ => unreachable!(),
        }
        assert!(draft.materialize().is_err(), "accepted empty {field}");
    }

    let skill = draft().materialize().unwrap();
    let markdown = skill.render_skill_markdown();
    for key in [
        "id: repo-hygiene",
        r#"title: "Repository hygiene""#,
        r#"summary: "Keep repository maintenance guidance current.""#,
        "category: maintenance",
        "targets: [cursor, codex]",
        "state: pending_approval",
        "pinned: false",
        "checksum: sha256:",
        "created_at: ",
        "updated_at: ",
        "provenance_source: automation_run",
        r#"provenance_actor: "tracedecay""#,
        r#"provenance_run_id: "run_123""#,
    ] {
        assert!(markdown.contains(key), "missing frontmatter key {key}");
    }

    let mut punctuated = draft();
    punctuated.title = "Review: automation runs".to_string();
    punctuated.summary = "Use #tags sparingly.".to_string();
    punctuated.provenance.actor = "trace:decay".to_string();
    punctuated.provenance.run_id = Some("run:123".to_string());
    let markdown = punctuated.materialize().unwrap().render_skill_markdown();
    assert!(markdown.contains(r#"title: "Review: automation runs""#));
    assert!(markdown.contains(r#"summary: "Use #tags sparingly.""#));
    assert!(markdown.contains(r#"provenance_actor: "trace:decay""#));
    assert!(markdown.contains(r#"provenance_run_id: "run:123""#));
}

#[test]
fn rejects_frontmatter_breaking_metadata_and_oversized_body() {
    let mut bad_title = draft();
    bad_title.title = "Bad\nTitle".to_string();
    assert!(bad_title.materialize().is_err());

    let mut bad_summary = draft();
    bad_summary.summary = " Bad summary".to_string();
    assert!(bad_summary.materialize().is_err());

    let mut bad_category = draft();
    bad_category.category = "Bad Category".to_string();
    assert!(bad_category.materialize().is_err());

    let mut bad_actor = draft();
    bad_actor.provenance.actor = "bad\nactor".to_string();
    assert!(bad_actor.materialize().is_err());

    let mut bad_run_id = draft();
    bad_run_id.provenance.run_id = Some("bad\nrun".to_string());
    assert!(bad_run_id.materialize().is_err());

    let mut nested_frontmatter = draft();
    nested_frontmatter.body_markdown =
        "---\nname: injected\ndescription: duplicate schema\n---\n\nUse this.".to_string();
    let err = nested_frontmatter.materialize().unwrap_err();
    assert!(err
        .to_string()
        .contains("body_markdown cannot include YAML frontmatter"));

    let mut oversized_body = draft();
    oversized_body.body_markdown = "x".repeat(MAX_MANAGED_SKILL_BODY_BYTES + 1);
    assert!(oversized_body.materialize().is_err());

    let mut no_targets = draft();
    no_targets.targets = Vec::new();
    assert!(no_targets.materialize().is_err());

    let mut duplicate_targets = draft();
    duplicate_targets.targets = vec![SkillInstallTarget::Codex, SkillInstallTarget::Codex];
    assert!(duplicate_targets.materialize().is_err());

    let mut hermes_target = draft();
    hermes_target.targets = vec![SkillInstallTarget::Hermes];
    assert!(hermes_target.materialize().is_err());
}

#[test]
fn checksum_is_deterministic_and_tracks_content_not_state_or_pin() {
    let mut first = draft().materialize().unwrap();
    let mut second = draft().materialize().unwrap();
    assert_eq!(first.metadata.checksum, second.metadata.checksum);

    first.set_state(ManagedSkillState::Active);
    first.set_pinned(true);
    assert_eq!(first.metadata.checksum, second.metadata.checksum);

    second.body_markdown.push_str("\nAdd one more rule.");
    second.refresh_checksum();
    assert_ne!(first.metadata.checksum, second.metadata.checksum);
}

#[test]
fn state_and_pin_lifecycle_is_explicit() {
    let mut skill = draft().materialize().unwrap();
    skill.metadata.updated_at = 1;
    assert_eq!(skill.metadata.state, ManagedSkillState::PendingApproval);
    assert!(!skill.metadata.pinned);

    skill.set_state(ManagedSkillState::Active);
    assert!(skill.metadata.updated_at > 1);
    skill.metadata.updated_at = 1;
    skill.set_pinned(true);
    assert!(skill.metadata.updated_at > 1);
    assert_eq!(skill.metadata.state, ManagedSkillState::Active);
    assert!(skill.metadata.pinned);

    skill.metadata.updated_at = 1;
    skill.set_state(ManagedSkillState::Disabled);
    assert!(skill.metadata.updated_at > 1);
    assert_eq!(skill.metadata.state, ManagedSkillState::Disabled);
    assert!(skill.metadata.pinned);
}

#[tokio::test]
async fn managed_skill_updates_reject_invalid_metadata_without_mutating_active_revision() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();
    let active = approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();

    let err = update_managed_skill(
        &profile_root,
        "repo-hygiene",
        ManagedSkillUpdate {
            summary: Some("Bad\nsummary".to_string()),
            ..ManagedSkillUpdate::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("must be a single line"));

    let reloaded = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(reloaded.metadata.summary, active.metadata.summary);
    assert_eq!(reloaded.metadata.state, ManagedSkillState::Active);
    assert_eq!(reloaded.metadata.checksum, active.metadata.checksum);
    assert!(reloaded.pending_update.is_none());

    let err = stage_managed_skill_update(
        &profile_root,
        "repo-hygiene",
        &active.metadata.checksum,
        ManagedSkillUpdate {
            category: Some("Bad Category".to_string()),
            targets: Some(vec![SkillInstallTarget::Claude]),
            ..ManagedSkillUpdate::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("lowercase letters, numbers, '-' or '_'"));

    let reloaded = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(reloaded.metadata.summary, active.metadata.summary);
    assert_eq!(reloaded.metadata.category, active.metadata.category);
    assert_eq!(reloaded.metadata.state, ManagedSkillState::Active);
    assert_eq!(reloaded.metadata.checksum, active.metadata.checksum);
    assert!(reloaded.pending_update.is_none());
    assert!(!managed_skill_dir(&profile_root, "repo-hygiene")
        .unwrap()
        .join("pending_update.json")
        .exists());

    let err = update_managed_skill(
        &profile_root,
        "repo-hygiene",
        ManagedSkillUpdate {
            body_markdown: Some("x".repeat(MAX_MANAGED_SKILL_BODY_BYTES + 1)),
            ..ManagedSkillUpdate::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("body_markdown exceeds"));

    let reloaded = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(reloaded.metadata.summary, active.metadata.summary);
    assert_eq!(reloaded.metadata.state, ManagedSkillState::Active);
    assert_eq!(reloaded.metadata.checksum, active.metadata.checksum);
    assert_eq!(reloaded.metadata.targets, active.metadata.targets);
    assert!(reloaded.pending_update.is_none());

    let err = stage_managed_skill_update(
        &profile_root,
        "repo-hygiene",
        &active.metadata.checksum,
        ManagedSkillUpdate {
            body_markdown: Some(
                " \n---\nname: injected\ndescription: duplicate schema\n---\n\nUse this."
                    .to_string(),
            ),
            ..ManagedSkillUpdate::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("body_markdown cannot include YAML frontmatter"));
    let reloaded = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(reloaded.metadata.checksum, active.metadata.checksum);
    assert!(reloaded.pending_update.is_none());
}

#[tokio::test]
async fn managed_skill_store_persists_package_and_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");

    let skill = create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();
    assert_eq!(skill.metadata.state, ManagedSkillState::PendingApproval);
    assert!(skill.metadata.created_at > 0);
    assert!(skill.metadata.updated_at >= skill.metadata.created_at);
    let skill_dir = managed_skill_dir(&profile_root, "repo-hygiene").unwrap();
    assert!(skill_dir.join("skill.json").is_file());
    assert!(skill_dir.join("SKILL.md").is_file());
    assert!(skill_dir.join("references/checklist.md").is_file());

    let active = set_managed_skill_state(&profile_root, "repo-hygiene", ManagedSkillState::Active)
        .await
        .unwrap();
    assert_eq!(active.metadata.state, ManagedSkillState::Active);
    assert_eq!(active.metadata.created_at, skill.metadata.created_at);
    assert!(active.metadata.updated_at >= skill.metadata.updated_at);
    let disabled =
        set_managed_skill_state(&profile_root, "repo-hygiene", ManagedSkillState::Disabled)
            .await
            .unwrap();
    assert_eq!(disabled.metadata.state, ManagedSkillState::Disabled);
    let archived =
        set_managed_skill_state(&profile_root, "repo-hygiene", ManagedSkillState::Archived)
            .await
            .unwrap();
    assert_eq!(archived.metadata.state, ManagedSkillState::Archived);

    let skills = list_managed_skills(&profile_root).await.unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].metadata.id, "repo-hygiene");
    assert_eq!(skills[0].metadata.state, ManagedSkillState::Archived);
}

#[tokio::test]
async fn managed_skill_store_updates_skill_markdown_on_state_change() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let mut skill = draft().materialize().unwrap();
    skill.set_pinned(true);
    save_managed_skill(&profile_root, &skill).await.unwrap();

    set_managed_skill_state(&profile_root, "repo-hygiene", ManagedSkillState::Active)
        .await
        .unwrap();
    let skill_md = std::fs::read_to_string(
        managed_skill_dir(&profile_root, "repo-hygiene")
            .unwrap()
            .join("SKILL.md"),
    )
    .unwrap();

    assert!(skill_md.contains("state: active"));
    assert!(skill_md.contains("pinned: true"));
    assert!(skill_md.contains("created_at: "));
    assert!(skill_md.contains("updated_at: "));
}

#[tokio::test]
async fn managed_skill_load_backfills_missing_timestamps() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let skill_dir = managed_skill_dir(&profile_root, "legacy-skill").unwrap();
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("skill.json"),
        r#"{
  "metadata": {
    "id": "legacy-skill",
    "title": "Legacy skill",
    "summary": "Old record before timestamps.",
    "category": "maintenance",
    "state": "active",
    "pinned": false,
    "checksum": "sha256:legacy",
    "provenance": {
      "source": "import",
      "actor": "test",
      "run_id": null
    }
  },
  "body_markdown": "Legacy body.",
  "support_files": []
}"#,
    )
    .unwrap();

    let skill = load_managed_skill(&profile_root, "legacy-skill")
        .await
        .unwrap();
    assert!(skill.metadata.created_at > 0);
    assert!(skill.metadata.updated_at > 0);

    std::fs::write(
        skill_dir.join("skill.json"),
        r#"{
  "metadata": {
    "id": "legacy-skill",
    "title": "Legacy skill",
    "summary": "Old record with only creation time.",
    "category": "maintenance",
    "state": "active",
    "pinned": false,
    "checksum": "sha256:legacy",
    "created_at": 100,
    "provenance": {
      "source": "import",
      "actor": "test",
      "run_id": null
    }
  },
  "body_markdown": "Legacy body.",
  "support_files": []
}"#,
    )
    .unwrap();
    let skill = load_managed_skill(&profile_root, "legacy-skill")
        .await
        .unwrap();
    assert_eq!(skill.metadata.created_at, 100);
    assert_eq!(skill.metadata.updated_at, 100);

    std::fs::write(
        skill_dir.join("skill.json"),
        r#"{
  "metadata": {
    "id": "legacy-skill",
    "title": "Legacy skill",
    "summary": "Old record with inconsistent timestamps.",
    "category": "maintenance",
    "state": "active",
    "pinned": false,
    "checksum": "sha256:legacy",
    "created_at": 200,
    "updated_at": 100,
    "provenance": {
      "source": "import",
      "actor": "test",
      "run_id": null
    }
  },
  "body_markdown": "Legacy body.",
  "support_files": []
}"#,
    )
    .unwrap();
    let skill = load_managed_skill(&profile_root, "legacy-skill")
        .await
        .unwrap();
    assert_eq!(skill.metadata.created_at, 200);
    assert_eq!(skill.metadata.updated_at, 200);
}

#[tokio::test]
async fn managed_skill_lifecycle_helpers_keep_activation_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();

    let active = approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(active.metadata.state, ManagedSkillState::Active);

    let disabled = disable_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(disabled.metadata.state, ManagedSkillState::Disabled);

    let archived = archive_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(archived.metadata.state, ManagedSkillState::Archived);

    let restored = restore_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(restored.metadata.state, ManagedSkillState::PendingApproval);
}

#[tokio::test]
async fn managed_skill_save_syncs_usage_lifecycle_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();

    let active = approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let summary = summarize_skill_usage_for(&profile_root, &active)
        .await
        .unwrap();
    assert_eq!(summary.skill_id, "repo-hygiene");
    assert_eq!(summary.title.as_deref(), Some("Repository hygiene"));
    assert_eq!(summary.category.as_deref(), Some("maintenance"));
    assert_eq!(summary.state, Some(ManagedSkillState::Active));
    assert_eq!(summary.created_by.as_deref(), Some("tracedecay"));
    assert_eq!(summary.view_count, 0);

    let disabled = disable_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let summary = summarize_skill_usage_for(&profile_root, &disabled)
        .await
        .unwrap();
    assert_eq!(summary.state, Some(ManagedSkillState::Disabled));
}

#[tokio::test]
async fn skill_usage_ledger_records_direct_and_analytics_events() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let skill = create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();

    record_skill_usage_event(
        &profile_root,
        SkillUsageEvent {
            skill_name: "repo-hygiene".to_string(),
            action: SkillUsageAction::View,
            timestamp: 100,
            target: Some("codex".to_string()),
        },
        Some(&skill),
    )
    .await
    .unwrap();

    let events = vec![
        AnalyticsEventRecord {
            id: 1,
            provider: "cursor".to_string(),
            project_id: "project".to_string(),
            session_id: None,
            timestamp: 110,
            event_kind: "skill".to_string(),
            hook_name: None,
            tool_name: None,
            tool_category: None,
            skill_name: Some("repo-hygiene".to_string()),
            hint_category: None,
            hint_id: None,
            outcome: None,
            metadata_json: Some(r#"{"skill":"repo-hygiene"}"#.to_string()),
        },
        AnalyticsEventRecord {
            id: 2,
            provider: "codex".to_string(),
            project_id: "project".to_string(),
            session_id: None,
            timestamp: 120,
            event_kind: "mcp_tool_call".to_string(),
            hook_name: None,
            tool_name: Some("skill_view".to_string()),
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: None,
            outcome: None,
            metadata_json: Some(
                r#"{"function":{"name":"skill_view","arguments":{"name":"repo-hygiene"}}}"#
                    .to_string(),
            ),
        },
        AnalyticsEventRecord {
            id: 3,
            provider: "codex".to_string(),
            project_id: "project".to_string(),
            session_id: None,
            timestamp: 130,
            event_kind: "skill".to_string(),
            hook_name: None,
            tool_name: None,
            tool_category: None,
            skill_name: Some("repo-hygiene".to_string()),
            hint_category: None,
            hint_id: None,
            outcome: Some("patched".to_string()),
            metadata_json: Some(r#"{"skill":"repo-hygiene"}"#.to_string()),
        },
    ];
    ingest_analytics_events(&profile_root, &events)
        .await
        .unwrap();

    let summary = summarize_skill_usage_for(&profile_root, &skill)
        .await
        .unwrap();
    assert_eq!(summary.view_count, 2);
    assert_eq!(summary.use_count, 1);
    assert_eq!(summary.patch_count, 1);
    assert_eq!(summary.last_viewed_at, Some(120));
    assert_eq!(summary.last_used_at, Some(110));
    assert_eq!(summary.last_patched_at, Some(130));
    assert_eq!(summary.last_activity_at, 130);
    assert_eq!(summary.targets, vec!["codex", "cursor"]);
}

#[tokio::test]
async fn managed_skill_update_restages_content_changes_for_approval() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();
    approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let mut before_update = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let created_at = before_update.metadata.created_at;
    before_update.metadata.updated_at = 1;
    save_managed_skill(&profile_root, &before_update)
        .await
        .unwrap();

    let updated = update_managed_skill(
        &profile_root,
        "repo-hygiene",
        ManagedSkillUpdate {
            summary: Some("Updated summary from review evidence.".to_string()),
            body_markdown: Some(
                "Run focused checks and record why they cover the change.".to_string(),
            ),
            pinned: Some(true),
            ..ManagedSkillUpdate::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(updated.metadata.state, ManagedSkillState::PendingApproval);
    assert!(updated.metadata.pinned);
    assert_eq!(updated.metadata.created_at, created_at);
    assert!(updated.metadata.updated_at > 1);
    assert_eq!(
        updated.metadata.summary,
        "Updated summary from review evidence."
    );

    let reloaded = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(
        reloaded.metadata.summary,
        "Keep repository maintenance guidance current."
    );
    assert_eq!(reloaded.metadata.state, ManagedSkillState::Active);
    assert_ne!(reloaded.metadata.checksum, updated.metadata.checksum);
    assert!(reloaded.pending_update.is_some());
    assert_eq!(
        reloaded.pending_update.as_ref().unwrap().metadata.summary,
        "Updated summary from review evidence."
    );
    assert!(!reloaded.render_skill_markdown().contains("Updated summary"));

    let approved = approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(approved.metadata.state, ManagedSkillState::Active);
    assert_eq!(approved.metadata.summary, updated.metadata.summary);
    assert_eq!(approved.metadata.checksum, updated.metadata.checksum);
    assert!(approved.render_skill_markdown().contains("Updated summary"));
}

#[tokio::test]
async fn managed_skill_update_removes_stale_support_files() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();
    let skill_dir = managed_skill_dir(&profile_root, "repo-hygiene").unwrap();
    assert!(skill_dir.join("references/checklist.md").is_file());

    update_managed_skill(
        &profile_root,
        "repo-hygiene",
        ManagedSkillUpdate {
            support_files: Some(vec![ManagedSupportFile::new(
                "templates/new.md",
                b"new body".to_vec(),
            )
            .unwrap()]),
            ..ManagedSkillUpdate::default()
        },
    )
    .await
    .unwrap();

    assert!(!skill_dir.join("references/checklist.md").exists());
    assert!(skill_dir.join("templates/new.md").is_file());
    assert!(skill_dir.join("skill.json").is_file());
    assert!(skill_dir.join("SKILL.md").is_file());
}

#[tokio::test]
async fn staged_managed_skill_update_preserves_active_revision_until_approval() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();
    let active = approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let base_checksum = active.metadata.checksum.clone();
    let skill_dir = managed_skill_dir(&profile_root, "repo-hygiene").unwrap();

    let staged = stage_managed_skill_update(
        &profile_root,
        "repo-hygiene",
        &base_checksum,
        ManagedSkillUpdate {
            summary: Some("Stage safer repository hygiene guidance.".to_string()),
            body_markdown: Some(
                "Review the run ledger before applying generated edits.".to_string(),
            ),
            support_files: Some(vec![ManagedSupportFile::new(
                "templates/review.md",
                b"review body".to_vec(),
            )
            .unwrap()]),
            ..ManagedSkillUpdate::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(staged.metadata.state, ManagedSkillState::PendingApproval);
    assert_eq!(
        staged.metadata.summary,
        "Stage safer repository hygiene guidance."
    );
    assert_ne!(staged.metadata.checksum, base_checksum);
    assert!(skill_dir.join("pending_update.json").is_file());
    assert!(skill_dir.join("references/checklist.md").is_file());
    assert!(!skill_dir.join("templates/review.md").exists());

    let active_with_pending = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(
        active_with_pending.metadata.state,
        ManagedSkillState::Active
    );
    assert_eq!(
        active_with_pending.metadata.summary,
        "Keep repository maintenance guidance current."
    );
    assert_eq!(active_with_pending.metadata.checksum, base_checksum);
    assert_eq!(
        active_with_pending
            .pending_update
            .as_ref()
            .unwrap()
            .metadata
            .summary,
        "Stage safer repository hygiene guidance."
    );

    let approved = approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(approved.metadata.state, ManagedSkillState::Active);
    assert_eq!(
        approved.metadata.summary,
        "Stage safer repository hygiene guidance."
    );
    assert!(approved.pending_update.is_none());
    assert!(!skill_dir.join("pending_update.json").exists());
    assert!(!skill_dir.join("references/checklist.md").exists());
    assert!(skill_dir.join("templates/review.md").is_file());

    let second_base = approved.metadata.checksum.clone();
    stage_managed_skill_update(
        &profile_root,
        "repo-hygiene",
        &second_base,
        ManagedSkillUpdate {
            summary: Some("Discard this staged update.".to_string()),
            ..ManagedSkillUpdate::default()
        },
    )
    .await
    .unwrap();
    let discarded = discard_pending_managed_skill_update(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert!(discarded.pending_update.is_none());
    let reloaded = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(
        reloaded.metadata.summary,
        "Stage safer repository hygiene guidance."
    );
    assert_eq!(reloaded.metadata.checksum, second_base);
}

#[tokio::test]
async fn staged_managed_skill_update_rejects_no_op_patch() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();
    let active = approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let err = stage_managed_skill_update(
        &profile_root,
        "repo-hygiene",
        &active.metadata.checksum,
        ManagedSkillUpdate {
            summary: Some(active.metadata.summary.clone()),
            body_markdown: Some(active.body_markdown.clone()),
            support_files: Some(active.support_files.clone()),
            ..ManagedSkillUpdate::default()
        },
    )
    .await
    .unwrap_err();

    assert!(err
        .to_string()
        .contains("update does not change the active revision"));
    let reloaded = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert!(reloaded.pending_update.is_none());
    assert!(!managed_skill_dir(&profile_root, "repo-hygiene")
        .unwrap()
        .join("pending_update.json")
        .exists());
}

#[tokio::test]
async fn managed_skill_usage_ledger_records_views_uses_and_patches() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let mut skill = create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();
    skill.set_pinned(true);
    save_managed_skill(&profile_root, &skill).await.unwrap();

    record_skill_usage(
        &profile_root,
        &skill,
        SkillUsageAction::View,
        "dashboard",
        vec!["Cursor".to_string(), "codex".to_string()],
        Some("cursor".to_string()),
        None,
    )
    .await
    .unwrap();
    record_skill_usage(
        &profile_root,
        &skill,
        SkillUsageAction::Use,
        "mcp",
        vec!["codex".to_string()],
        Some("codex".to_string()),
        None,
    )
    .await
    .unwrap();
    record_skill_usage(
        &profile_root,
        &skill,
        SkillUsageAction::Patch,
        "automation",
        vec!["cursor".to_string()],
        None,
        Some(serde_json::json!({"reason": "test"})),
    )
    .await
    .unwrap();

    assert!(skill_usage_ledger_path(&profile_root).is_file());
    let records = load_skill_usage_records(&profile_root, None).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].skill_id, "repo-hygiene");
    assert_eq!(records[0].state, Some(ManagedSkillState::PendingApproval));
    assert!(records[0].pinned);
    assert_eq!(records[0].view_count, 1);
    assert_eq!(records[0].use_count, 1);
    assert_eq!(records[0].patch_count, 1);

    let summaries = summarize_skill_usage(&profile_root, &[skill])
        .await
        .unwrap();
    assert_eq!(summaries.len(), 1);
    let summary = &summaries[0];
    assert_eq!(summary.skill_id, "repo-hygiene");
    assert_eq!(summary.view_count, 1);
    assert_eq!(summary.use_count, 1);
    assert_eq!(summary.patch_count, 1);
    assert_eq!(summary.targets, vec!["codex", "cursor"]);
    assert!(summary.last_activity_at > 0);
}

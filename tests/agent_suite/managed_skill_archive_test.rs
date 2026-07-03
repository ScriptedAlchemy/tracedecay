use tracedecay::automation::managed_skills::{
    approve_managed_skill, archive_managed_skill, create_managed_skill_draft,
    discard_pending_managed_skill_update, load_managed_skill, managed_skill_dir,
    save_managed_skill, stage_managed_skill_archive, ManagedSkillDraft, ManagedSkillProvenance,
    ManagedSkillSource, ManagedSkillState, ManagedSupportFile, SkillInstallTarget,
};

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

#[tokio::test]
async fn staged_managed_skill_archive_keeps_content_until_approval() {
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

    let staged = stage_managed_skill_archive(
        &profile_root,
        "repo-hygiene",
        &base_checksum,
        Some("overlaps with newer guidance".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(staged.metadata.state, ManagedSkillState::PendingApproval);

    let with_pending = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(with_pending.metadata.state, ManagedSkillState::Active);
    assert_eq!(with_pending.metadata.checksum, base_checksum);
    let pending = with_pending.pending_update.as_ref().unwrap();
    assert_eq!(pending.resulting_state, Some(ManagedSkillState::Archived));
    assert_eq!(
        pending.staged_reason.as_deref(),
        Some("overlaps with newer guidance")
    );

    let approved = approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(approved.metadata.state, ManagedSkillState::Archived);
    assert_eq!(approved.body_markdown, active.body_markdown);
    assert!(approved.pending_update.is_none());
    assert!(skill_dir.join("SKILL.md").is_file());
    assert!(skill_dir.join("references/checklist.md").is_file());
    let reloaded = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(reloaded.metadata.state, ManagedSkillState::Archived);
    assert_eq!(reloaded.body_markdown, active.body_markdown);
}

#[tokio::test]
async fn staged_managed_skill_archive_can_be_discarded() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();
    let active = approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    stage_managed_skill_archive(
        &profile_root,
        "repo-hygiene",
        &active.metadata.checksum,
        None,
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
    assert_eq!(reloaded.metadata.state, ManagedSkillState::Active);
    assert_eq!(reloaded.metadata.checksum, active.metadata.checksum);
}

#[tokio::test]
async fn staged_managed_skill_archive_rejects_pinned_stale_and_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    create_managed_skill_draft(&profile_root, draft())
        .await
        .unwrap();
    let mut active = approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let base_checksum = active.metadata.checksum.clone();

    let err = stage_managed_skill_archive(&profile_root, "repo-hygiene", "sha256:stale", None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("is stale"));

    active.set_pinned(true);
    save_managed_skill(&profile_root, &active).await.unwrap();
    let err = stage_managed_skill_archive(&profile_root, "repo-hygiene", &base_checksum, None)
        .await
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("pinned and exempt from staged archive"));

    active.set_pinned(false);
    save_managed_skill(&profile_root, &active).await.unwrap();
    stage_managed_skill_archive(&profile_root, "repo-hygiene", &base_checksum, None)
        .await
        .unwrap();
    let err = stage_managed_skill_archive(&profile_root, "repo-hygiene", &base_checksum, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already has a pending update"));

    discard_pending_managed_skill_update(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let archived = archive_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    let err = stage_managed_skill_archive(
        &profile_root,
        "repo-hygiene",
        &archived.metadata.checksum,
        None,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("is already archived"));
}

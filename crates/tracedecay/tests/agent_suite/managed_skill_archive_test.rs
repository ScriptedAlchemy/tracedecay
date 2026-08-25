use tracedecay_agent_hosts::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState,
    ManagedSupportFile, SkillInstallTarget, apply_managed_skill_archive, create_managed_skill,
    load_managed_skill, managed_skill_dir, set_managed_skill_pinned,
};

fn draft() -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: "repo-hygiene".to_string(),
        title: "Repository hygiene".to_string(),
        summary: "Keep repository maintenance guidance current.".to_string(),
        category: "maintenance".to_string(),
        targets: vec![SkillInstallTarget::Cursor, SkillInstallTarget::Codex],
        body_markdown: "Use focused checks before changing generated files.".to_string(),
        support_files: vec![
            ManagedSupportFile::new(
                "references/checklist.md",
                b"- check dirty tree\n- run focused tests\n".to_vec(),
            )
            .unwrap(),
        ],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "tracedecay".to_string(),
            run_id: Some("run_123".to_string()),
        },
    }
}

#[tokio::test]
async fn automatic_managed_skill_archive_activates_the_archived_revision_immediately() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let active = create_managed_skill(&profile_root, draft()).await.unwrap();
    let base_checksum = active.metadata.checksum.clone();
    let skill_dir = managed_skill_dir(&profile_root, "repo-hygiene").unwrap();

    let archived = apply_managed_skill_archive(
        &profile_root,
        "repo-hygiene",
        &base_checksum,
        Some("overlaps with newer guidance".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(archived.metadata.state, ManagedSkillState::Archived);
    assert_eq!(
        archived.metadata.archived_reason.as_deref(),
        Some("overlaps with newer guidance")
    );
    assert_eq!(archived.body_markdown, active.body_markdown);
    assert!(skill_dir.join("SKILL.md").is_file());
    assert!(skill_dir.join("references/checklist.md").is_file());
    let reloaded = load_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();
    assert_eq!(reloaded.metadata.state, ManagedSkillState::Archived);
    assert_eq!(reloaded.body_markdown, active.body_markdown);
}

#[tokio::test]
async fn automatic_managed_skill_archive_rejects_pinned_stale_and_archived_revisions() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let active = create_managed_skill(&profile_root, draft()).await.unwrap();
    let base_checksum = active.metadata.checksum.clone();

    let err = apply_managed_skill_archive(&profile_root, "repo-hygiene", "sha256:stale", None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("is stale"));

    set_managed_skill_pinned(&profile_root, "repo-hygiene", true)
        .await
        .unwrap();
    let err = apply_managed_skill_archive(&profile_root, "repo-hygiene", &base_checksum, None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("pinned and exempt from automatic archive")
    );

    set_managed_skill_pinned(&profile_root, "repo-hygiene", false)
        .await
        .unwrap();
    let archived = apply_managed_skill_archive(&profile_root, "repo-hygiene", &base_checksum, None)
        .await
        .unwrap();
    let err = apply_managed_skill_archive(
        &profile_root,
        "repo-hygiene",
        &archived.metadata.checksum,
        None,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("is already archived"));
}

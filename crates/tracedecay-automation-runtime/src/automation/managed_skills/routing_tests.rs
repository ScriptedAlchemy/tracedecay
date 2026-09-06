use super::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState,
    ManagedSkillUpdate, ManagedSupportFile, SkillInstallTarget, apply_managed_skill_update,
    create_managed_skill, decode_retained_skill, list_managed_skills, load_managed_skill,
    managed_skill_dir, migrate_managed_skill_routing, set_managed_skill_pinned,
    set_managed_skill_state,
};
use tracedecay_automation::managed_skills::legacy_managed_skill_routing_description;

// SHA-256 of this fixture's historical content encoding: id, title, summary,
// category, target tags, body, and support file path/bytes, without routing text.
const HISTORICAL_CHECKSUM: &str =
    "sha256:566d3d67af3e00c94b623ede09c424261f796988ae458803cc09171143572e6c";

fn routing_draft() -> ManagedSkillDraft {
    let summary = r#"Diagnose "quoted" paths"#;
    ManagedSkillDraft {
        id: "legacy-routing".to_string(),
        title: "Legacy routing".to_string(),
        summary: summary.to_string(),
        routing_description: legacy_managed_skill_routing_description(summary),
        category: "testing".to_string(),
        targets: vec![SkillInstallTarget::Codex, SkillInstallTarget::Claude],
        body_markdown: "# Routing\n\nInspect the failing path.\n".to_string(),
        support_files: vec![
            ManagedSupportFile::new("references/paths.md", b"Preserve path identity.\n".to_vec())
                .unwrap(),
        ],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "routing-migration-test".to_string(),
            run_id: Some("retained-run".to_string()),
        },
    }
}

#[tokio::test]
async fn retained_routing_migration_preserves_exports_and_fences_old_checksums() {
    let profile = tempfile::TempDir::new().unwrap();
    let created = create_managed_skill(profile.path(), routing_draft())
        .await
        .unwrap();
    let id = &created.metadata.id;
    set_managed_skill_state(profile.path(), id, ManagedSkillState::Disabled)
        .await
        .unwrap();
    let current = set_managed_skill_pinned(profile.path(), id, true)
        .await
        .unwrap();
    let expected_native = current.render_native_skill_markdown().unwrap();
    let expected_materialized = current.render_materialized_skill_markdown().unwrap();
    let dir = managed_skill_dir(profile.path(), id).unwrap();
    let record = dir.join("skill.json");
    let mut legacy_value = serde_json::to_value(&current).unwrap();
    legacy_value["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("routing_description");
    legacy_value["metadata"]["checksum"] = HISTORICAL_CHECKSUM.into();
    let legacy_bytes = serde_json::to_vec_pretty(&legacy_value).unwrap();
    std::fs::write(&record, &legacy_bytes).unwrap();
    let stored_markdown = std::fs::read(dir.join("SKILL.md")).unwrap();

    let mut expected_legacy = current.clone();
    expected_legacy.metadata.checksum = HISTORICAL_CHECKSUM.to_string();
    assert_eq!(
        load_managed_skill(profile.path(), id).await.unwrap(),
        expected_legacy
    );
    assert_eq!(
        list_managed_skills(profile.path()).await.unwrap(),
        vec![expected_legacy]
    );
    assert_eq!(std::fs::read(&record).unwrap(), legacy_bytes);
    assert_eq!(
        std::fs::read(dir.join("SKILL.md")).unwrap(),
        stored_markdown
    );

    migrate_managed_skill_routing(profile.path()).await.unwrap();
    let migrated = load_managed_skill(profile.path(), id).await.unwrap();
    // Full equality covers state, pin, timestamps, provenance, body, and support.
    assert_eq!(migrated, current);
    assert_eq!(
        migrated.render_native_skill_markdown().unwrap(),
        expected_native
    );
    assert_eq!(
        migrated.render_materialized_skill_markdown().unwrap(),
        expected_materialized
    );
    assert_eq!(
        std::fs::read(dir.join("references/paths.md")).unwrap(),
        b"Preserve path identity.\n"
    );
    let migrated_bytes = std::fs::read(&record).unwrap();
    assert_ne!(migrated_bytes, legacy_bytes);
    let (decoded, needs_migration) = decode_retained_skill(&migrated_bytes).unwrap();
    assert!(!needs_migration);
    assert_eq!(decoded, migrated);
    migrate_managed_skill_routing(profile.path()).await.unwrap();
    assert_eq!(std::fs::read(&record).unwrap(), migrated_bytes);
    assert_eq!(
        load_managed_skill(profile.path(), id).await.unwrap(),
        migrated
    );

    let update = ManagedSkillUpdate {
        routing_description: Some("Diagnose Windows path quoting failures.".to_string()),
        ..Default::default()
    };
    let error = apply_managed_skill_update(profile.path(), id, HISTORICAL_CHECKSUM, update.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("base_checksum"));
    assert!(error.to_string().contains("stale"));
    assert_eq!(std::fs::read(&record).unwrap(), migrated_bytes);
    let updated =
        apply_managed_skill_update(profile.path(), id, &migrated.metadata.checksum, update)
            .await
            .unwrap();
    assert_eq!(
        updated.metadata.routing_description,
        "Diagnose Windows path quoting failures."
    );
    assert_ne!(updated.metadata.checksum, migrated.metadata.checksum);
    assert_eq!(
        load_managed_skill(profile.path(), id).await.unwrap(),
        updated
    );
}

#[tokio::test]
async fn present_invalid_routing_is_rejected_without_legacy_fallback() {
    for invalid in [
        serde_json::Value::Null,
        serde_json::Value::String(String::new()),
    ] {
        let profile = tempfile::TempDir::new().unwrap();
        let skill = create_managed_skill(profile.path(), routing_draft())
            .await
            .unwrap();
        let dir = managed_skill_dir(profile.path(), &skill.metadata.id).unwrap();
        let record = dir.join("skill.json");
        let mut value = serde_json::to_value(&skill).unwrap();
        value["metadata"]["routing_description"] = invalid;
        let bytes = serde_json::to_vec_pretty(&value).unwrap();
        std::fs::write(&record, &bytes).unwrap();
        let markdown = std::fs::read(dir.join("SKILL.md")).unwrap();

        assert!(decode_retained_skill(&bytes).is_err());
        assert!(
            load_managed_skill(profile.path(), &skill.metadata.id)
                .await
                .is_err()
        );
        assert!(list_managed_skills(profile.path()).await.is_err());
        assert!(migrate_managed_skill_routing(profile.path()).await.is_err());
        assert_eq!(std::fs::read(&record).unwrap(), bytes);
        assert_eq!(std::fs::read(dir.join("SKILL.md")).unwrap(), markdown);
    }
}

#[tokio::test]
async fn legacy_lifecycle_mutations_complete_routing_checksum_cutover() {
    for operation in ["pin", "state", "pinned-only update"] {
        let profile = tempfile::TempDir::new().unwrap();
        let current = create_managed_skill(profile.path(), routing_draft())
            .await
            .unwrap();
        let id = &current.metadata.id;
        let record = managed_skill_dir(profile.path(), id)
            .unwrap()
            .join("skill.json");
        let mut legacy = serde_json::to_value(&current).unwrap();
        legacy["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("routing_description");
        legacy["metadata"]["checksum"] = HISTORICAL_CHECKSUM.into();
        std::fs::write(&record, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let updated = match operation {
            "pin" => set_managed_skill_pinned(profile.path(), id, true)
                .await
                .unwrap(),
            "state" => set_managed_skill_state(profile.path(), id, ManagedSkillState::Disabled)
                .await
                .unwrap(),
            _ => apply_managed_skill_update(
                profile.path(),
                id,
                HISTORICAL_CHECKSUM,
                ManagedSkillUpdate {
                    pinned: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        };
        assert_eq!(
            updated.metadata.checksum, current.metadata.checksum,
            "{operation}"
        );
        assert_ne!(
            updated.metadata.checksum, HISTORICAL_CHECKSUM,
            "{operation}"
        );
        assert_eq!(
            updated.metadata.routing_description,
            current.metadata.routing_description
        );
        assert_eq!(updated.metadata.provenance, current.metadata.provenance);
        assert_eq!(updated.body_markdown, current.body_markdown);
        assert_eq!(updated.support_files, current.support_files);
        if operation == "state" {
            assert_eq!(updated.metadata.state, ManagedSkillState::Disabled);
        } else {
            assert!(updated.metadata.pinned);
        }
        assert_eq!(
            load_managed_skill(profile.path(), id).await.unwrap(),
            updated
        );
        let bytes = std::fs::read(&record).unwrap();
        let (decoded, needs_migration) = decode_retained_skill(&bytes).unwrap();
        assert!(!needs_migration);
        assert_eq!(decoded, updated);
        migrate_managed_skill_routing(profile.path()).await.unwrap();
        assert_eq!(std::fs::read(&record).unwrap(), bytes);
        assert_eq!(
            load_managed_skill(profile.path(), id).await.unwrap(),
            updated
        );

        let error = apply_managed_skill_update(
            profile.path(),
            id,
            HISTORICAL_CHECKSUM,
            ManagedSkillUpdate {
                routing_description: Some("Diagnose path escaping failures.".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("stale"), "{operation}: {error}");
        assert_eq!(std::fs::read(&record).unwrap(), bytes);
    }
}

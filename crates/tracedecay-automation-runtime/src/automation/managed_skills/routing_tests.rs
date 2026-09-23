use super::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSupportFile,
    SkillInstallTarget, create_managed_skill, list_managed_skills, load_managed_skill,
    managed_skill_dir,
};

fn routing_draft() -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: "routing".to_string(),
        title: "Routing".to_string(),
        summary: r#"Diagnose "quoted" paths"#.to_string(),
        routing_description: r#"Use when diagnosing "quoted" paths"#.to_string(),
        category: "testing".to_string(),
        targets: vec![SkillInstallTarget::Codex, SkillInstallTarget::Claude],
        body_markdown: "# Routing\n\nInspect the failing path.\n".to_string(),
        support_files: vec![
            ManagedSupportFile::new("references/paths.md", b"Preserve path identity.\n".to_vec())
                .unwrap(),
        ],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "routing-test".to_string(),
            run_id: Some("run".to_string()),
        },
    }
}

#[tokio::test]
async fn missing_or_invalid_routing_is_rejected_without_rewriting_records() {
    for invalid in [
        None,
        Some(serde_json::Value::Null),
        Some(serde_json::Value::String(String::new())),
    ] {
        let profile = tempfile::TempDir::new().unwrap();
        let skill = create_managed_skill(profile.path(), routing_draft())
            .await
            .unwrap();
        let dir = managed_skill_dir(profile.path(), &skill.metadata.id).unwrap();
        let record = dir.join("skill.json");
        let mut value = serde_json::to_value(&skill).unwrap();
        let metadata = value["metadata"].as_object_mut().unwrap();
        match invalid {
            Some(invalid) => {
                metadata.insert("routing_description".to_string(), invalid);
            }
            None => {
                metadata.remove("routing_description");
            }
        }
        let bytes = serde_json::to_vec_pretty(&value).unwrap();
        std::fs::write(&record, &bytes).unwrap();
        let markdown = std::fs::read(dir.join("SKILL.md")).unwrap();

        assert!(
            load_managed_skill(profile.path(), &skill.metadata.id)
                .await
                .is_err()
        );
        assert!(list_managed_skills(profile.path()).await.is_err());
        assert_eq!(std::fs::read(&record).unwrap(), bytes);
        assert_eq!(std::fs::read(dir.join("SKILL.md")).unwrap(), markdown);
    }
}

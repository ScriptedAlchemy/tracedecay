use tracedecay_domain::errors::TraceDecayError;

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
async fn released_summary_only_record_is_refused_and_the_store_scan_resets_only_it() {
    let profile = tempfile::TempDir::new().unwrap();
    let skill = create_managed_skill(profile.path(), routing_draft())
        .await
        .unwrap();
    create_managed_skill(
        profile.path(),
        ManagedSkillDraft {
            id: "keeper".to_string(),
            ..routing_draft()
        },
    )
    .await
    .unwrap();
    let dir = managed_skill_dir(profile.path(), &skill.metadata.id).unwrap();
    let record = dir.join("skill.json");
    let mut value = serde_json::to_value(&skill).unwrap();
    value["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("routing_description");
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    std::fs::write(&record, &bytes).unwrap();

    let refusal = load_managed_skill(profile.path(), "routing")
        .await
        .unwrap_err();
    let expected_reason = format!(
        "managed skill record '{}' is the released summary-only shape",
        record.display()
    );
    assert_eq!(
        refusal.reset_required_context(),
        Some(("managed skill store", expected_reason.as_str()))
    );
    assert_eq!(
        std::fs::read(&record).unwrap(),
        bytes,
        "a single-record read refuses without rewriting"
    );

    let listed = list_managed_skills(profile.path()).await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|skill| skill.metadata.id.as_str())
            .collect::<Vec<_>>(),
        ["keeper"]
    );
    assert!(!dir.exists(), "the refused skill directory is reset");
    assert_eq!(
        load_managed_skill(profile.path(), "routing")
            .await
            .unwrap_err()
            .to_string(),
        "config error: managed skill 'routing' not found"
    );
}

#[tokio::test]
async fn invalid_routing_is_rejected_without_rewriting_records() {
    for (invalid, expected_error) in [
        (
            Some(serde_json::Value::Null),
            "invalid type: null, expected a string",
        ),
        (
            Some(serde_json::Value::String(String::new())),
            "native description cannot be empty",
        ),
    ] {
        let profile = tempfile::TempDir::new().unwrap();
        let skill = create_managed_skill(profile.path(), routing_draft())
            .await
            .unwrap();
        let loaded = load_managed_skill(profile.path(), &skill.metadata.id)
            .await
            .unwrap();
        assert_eq!(
            loaded.metadata.routing_description,
            r#"Use when diagnosing "quoted" paths"#
        );
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

        for error in [
            load_managed_skill(profile.path(), &skill.metadata.id)
                .await
                .unwrap_err(),
            list_managed_skills(profile.path()).await.unwrap_err(),
        ] {
            assert!(
                matches!(&error, TraceDecayError::Config { message } if message.contains(expected_error)),
                "unexpected rejection: {error:?}"
            );
        }
        assert_eq!(std::fs::read(&record).unwrap(), bytes);
        assert_eq!(std::fs::read(dir.join("SKILL.md")).unwrap(), markdown);
    }
}

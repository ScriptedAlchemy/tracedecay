#[cfg(feature = "test-transport")]
use crate::support::*;

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_runner_auto_applies_safe_consolidations() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    let automation_provenance = || ManagedSkillProvenance {
        source: ManagedSkillSource::AutomationRun,
        actor: "skill_writer".to_string(),
        run_id: Some("run_seed".to_string()),
    };
    let target = create_managed_skill(
        &profile_root,
        ManagedSkillDraft {
            id: "automation-run-review".to_string(),
            title: "Automation run review".to_string(),
            summary: "Review automation run ledgers before applying changes.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown:
                "Check run ledger counts, rejected proposals, and validation evidence before applying automation changes."
                    .to_string(),
            support_files: Vec::new(),
            provenance: automation_provenance(),
        },
    )
    .await
    .unwrap();
    let source = create_managed_skill(
        &profile_root,
        ManagedSkillDraft {
            id: "automation-run-checks".to_string(),
            title: "Automation run checks".to_string(),
            summary: "Review automation run ledgers and validation gates.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown:
                "Check run ledger counts, rejected proposals, and validation gates before applying automation changes."
                    .to_string(),
            support_files: Vec::new(),
            provenance: automation_provenance(),
        },
    )
    .await
    .unwrap();
    let pinned = create_managed_skill(
        &profile_root,
        ManagedSkillDraft {
            id: "pinned-automation-guide".to_string(),
            title: "Pinned deployment guide".to_string(),
            summary: "Deployment rollback runbook kept pinned by the user.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: "Roll back with the canary checklist and notify the release channel."
                .to_string(),
            support_files: Vec::new(),
            provenance: automation_provenance(),
        },
    )
    .await
    .unwrap();
    tracedecay::automation::managed_skills::set_managed_skill_pinned(
        &profile_root,
        &pinned.metadata.id,
        true,
    )
    .await
    .unwrap();

    let backend = SequentialJsonBackend::new(vec![
        json!({
            "skills": [
            {
                "action": "merge",
                "id": "automation-run-review",
                "base_checksum": target.metadata.checksum.clone(),
                "source_skill_id": "automation-run-checks",
                "source_base_checksum": source.metadata.checksum.clone(),
                "body_markdown": "Check run ledger counts, rejected proposals, validation gates, and evidence receipts before applying automation changes.",
                "reason": "The two run-review skills overlap almost completely."
            },
            {
                "action": "archive",
                "id": "pinned-automation-guide",
                "base_checksum": pinned.metadata.checksum.clone(),
                "reason": "Pinned skills must be rejected."
            }
            ]
        }),
        json!({
            "skills": [{
                "action": "merge",
                "id": "automation-run-review",
                "base_checksum": target.metadata.checksum.clone(),
                "source_skill_id": "automation-run-checks",
                "source_base_checksum": source.metadata.checksum.clone(),
                "body_markdown": "Check run ledger counts, rejected proposals, validation gates, and evidence receipts before applying automation changes.",
                "reason": "The two run-review skills overlap almost completely."
            }]
        }),
    ]);
    let config = enabled_skill_writer_config();

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        manual_skill_writer_options(&profile_root),
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 2);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 0);
    assert_eq!(run.report["status"], json!("applied"));
    assert_eq!(run.report["validation_repairs"][0]["attempt"], json!(1));

    let consolidation = &run.report["applied_consolidations"][0];
    assert_eq!(consolidation["action"], json!("merge"));
    assert_eq!(consolidation["activation_status"], json!("applied"));
    assert_eq!(
        consolidation["target_skill_id"],
        json!("automation-run-review")
    );
    assert_eq!(
        consolidation["source_skill_id"],
        json!("automation-run-checks")
    );
    assert_eq!(
        consolidation["archived_skill_id"],
        json!("automation-run-checks")
    );
    assert_eq!(consolidation["resulting_state"], json!("archived"));
    assert_eq!(consolidation["target_update_applied"], json!(true));
    assert_eq!(run.report["rejected_skills"], json!([]));

    assert!(
        run.report["skill_improvement_recommendations"]
            .as_array()
            .is_some_and(
                |recommendations| recommendations.iter().any(|recommendation| {
                    recommendation["kind"] == "managed_skill_consolidation"
                        && recommendation["source"] == "skill_overlap_detection"
                })
            )
    );

    let archived_source = load_managed_skill(&profile_root, "automation-run-checks")
        .await
        .unwrap();
    assert_eq!(archived_source.metadata.state, ManagedSkillState::Archived);
    assert_eq!(
        archived_source.metadata.absorbed_into.as_deref(),
        Some("automation-run-review")
    );
    assert!(
        archived_source
            .metadata
            .archived_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("overlap"))
    );
    let updated_target = load_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    assert_eq!(updated_target.metadata.state, ManagedSkillState::Active);
    assert_ne!(updated_target.metadata.checksum, target.metadata.checksum);
    let untouched_pinned = load_managed_skill(&profile_root, "pinned-automation-guide")
        .await
        .unwrap();
    assert_eq!(untouched_pinned.metadata.state, ManagedSkillState::Active);

    assert_eq!(archived_source.body_markdown, source.body_markdown);
    assert_eq!(
        updated_target.body_markdown,
        "Check run ledger counts, rejected proposals, validation gates, and evidence receipts before applying automation changes."
    );
}

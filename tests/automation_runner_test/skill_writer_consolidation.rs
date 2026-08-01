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
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "automation-run-review".to_string(),
            title: "Automation run review".to_string(),
            summary: "Review automation run ledgers before approving changes.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown:
                "Check run ledger counts, rejected proposals, and pending approval state before applying automation changes."
                    .to_string(),
            support_files: Vec::new(),
            provenance: automation_provenance(),
        },
    )
    .await
    .unwrap();
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "automation-run-checks".to_string(),
            title: "Automation run checks".to_string(),
            summary: "Review automation run ledgers and approval gates.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown:
                "Check run ledger counts, rejected proposals, and approval gates before applying automation changes."
                    .to_string(),
            support_files: Vec::new(),
            provenance: automation_provenance(),
        },
    )
    .await
    .unwrap();
    create_managed_skill_draft(
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
    let target = approve_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    let source = approve_managed_skill(&profile_root, "automation-run-checks")
        .await
        .unwrap();
    let pinned = approve_managed_skill(&profile_root, "pinned-automation-guide")
        .await
        .unwrap();
    tracedecay::automation::managed_skills::set_managed_skill_pinned(
        &profile_root,
        &pinned.metadata.id,
        true,
    )
    .await
    .unwrap();

    let backend = SkillJsonBackend::with_activation_policy(
        json!({
            "skills": [
                {
                    "action": "merge",
                    "id": "automation-run-review",
                    "base_checksum": target.metadata.checksum,
                    "source_skill_id": "automation-run-checks",
                    "source_base_checksum": source.metadata.checksum,
                    "body_markdown": "Check run ledger counts, rejected proposals, approval gates, and pending approval state before applying automation changes.",
                    "reason": "The two run-review skills overlap almost completely."
                },
                {
                    "action": "archive",
                    "id": "pinned-automation-guide",
                    "base_checksum": pinned.metadata.checksum,
                    "reason": "Pinned skills must be rejected."
                }
            ]
        }),
        "auto_enable_after_validation",
    );
    let config = AutomationConfig {
        auto_enable_skills: true,
        ..enabled_skill_writer_config()
    };

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        manual_skill_writer_options(&profile_root),
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 1);

    let consolidation = &run.report["staged_consolidations"][0];
    assert_eq!(consolidation["action"], json!("merge"));
    assert_eq!(consolidation["approval_status"], json!("auto_applied"));
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
    assert_eq!(consolidation["target_update_staged"], json!(false));
    assert!(
        run.report["rejected_skills"][0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("pinned"))
    );

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

    let staged_source = load_managed_skill(&profile_root, "automation-run-checks")
        .await
        .unwrap();
    assert_eq!(staged_source.metadata.state, ManagedSkillState::Archived);
    assert_eq!(
        staged_source.metadata.absorbed_into.as_deref(),
        Some("automation-run-review")
    );
    assert!(
        staged_source
            .metadata
            .archived_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("overlap"))
    );
    let staged_target = load_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    assert_eq!(staged_target.metadata.state, ManagedSkillState::Active);
    assert_ne!(staged_target.metadata.checksum, target.metadata.checksum);
    assert!(staged_target.pending_update.is_none());
    let untouched_pinned = load_managed_skill(&profile_root, "pinned-automation-guide")
        .await
        .unwrap();
    assert_eq!(untouched_pinned.metadata.state, ManagedSkillState::Active);
    assert!(untouched_pinned.pending_update.is_none());

    assert_eq!(staged_source.body_markdown, source.body_markdown);
    assert!(staged_target.body_markdown.contains("approval gates"));
}

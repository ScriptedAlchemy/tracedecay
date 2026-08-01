use crate::support::*;

#[test]
fn skill_writer_options_have_no_storage_selector() {
    let options = serde_json::to_value(SkillWriterAutomationOptions::default()).unwrap();
    assert!(options.get("storage_scope").is_none());
    assert!(options.get("hermes_home").is_none());
    assert!(
        serde_json::from_value::<SkillWriterAutomationOptions>(json!({
            "hermes_home": "/tmp/hermes"
        }))
        .is_err()
    );
}

#[tokio::test]
async fn skill_writer_runner_skips_when_task_is_disabled() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let backend = SkillJsonBackend::new(json!({"skills": []}));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        ..AutomationConfig::default()
    };

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            profile_root: Some(temp.path().join("profile")),
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.task, AgentTaskKind::SkillWriter);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("skill_writer_disabled")
    );
}

#[tokio::test]
async fn skill_writer_fails_closed_on_denied_temporal_evidence() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    let backend = SkillJsonBackend::new(json!({"skills": []}));
    let retrieval = RejectedAutomationSessionRetrieval::new("session_evidence_denied");

    let run = tracedecay::automation::runner::run_skill_writer_with_backend_and_retrieval(
        &cg,
        &enabled_skill_writer_config(),
        &backend,
        &retrieval,
        SkillWriterAutomationOptions {
            profile_root: Some(profile_root.clone()),
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("session_evidence_denied")
    );
    assert!(
        load_run_records(&cg.store_layout().dashboard_root, 10)
            .await
            .unwrap()
            .is_empty(),
        "denied evidence must not write a ledger record"
    );
    assert!(
        !profile_root.exists(),
        "denied evidence must not create the managed-skill profile"
    );
    assert!(
        !cg.store_layout()
            .dashboard_root
            .join("automation_outcomes.json")
            .exists(),
        "denied evidence must not refresh skill outcomes"
    );
}

// Every test below that reaches evidence building holds `ENV_LOCK` and pins
// its profile database override at the isolated session store.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_default_provider_searches_all_providers() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    let db = project_session_runtime(&cg).await;
    seed_session_message_in_db(
        &db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "codex",
            session_id: "skill-writer-codex-default",
            message_id: "skill-writer-codex-default-message-001",
            role: "assistant",
            timestamp: 1_715_000_001,
            text: "Codex workflow correction repeated skill tool pattern evidence should be found by the default skill writer provider.",
            source: None,
        },
    )
    .await;
    let _global_db = isolate_global_db(&cg);
    let backend = SkillJsonBackend::new(json!({"skills": []}));
    let config = enabled_skill_writer_config();

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            profile_root: Some(profile_root),
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_replays_recent_sessions_without_keyword_matches() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    let db = project_session_runtime(&cg).await;
    // Deliberately avoids every keyword in the default skill writer query so
    // the grep channel returns nothing and only session replay surfaces it.
    seed_session_message_in_db(
        &db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "skill-writer-replay-1",
            message_id: "skill-writer-replay-1-message-001",
            role: "assistant",
            timestamp: 1_715_000_070,
            text: "Ran the dashboard build twice before every release cut.",
            source: None,
        },
    )
    .await;
    let _global_db = isolate_global_db(&cg);
    let backend = SkillWriterReplayEvidenceBackend::new("skill-writer-replay-1");
    let config = enabled_skill_writer_config();
    let retrieval = StaticAutomationSessionRetrieval::message(
        "skill-writer-replay-1",
        "skill-writer-replay-1-message-001",
        "Ran the dashboard build twice before every release cut.",
    );

    let run = tracedecay::automation::runner::run_skill_writer_with_backend_and_retrieval(
        &cg,
        &config,
        &backend,
        &retrieval,
        SkillWriterAutomationOptions {
            profile_root: Some(profile_root),
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
}

#[tokio::test]
async fn skill_writer_skips_when_replay_disabled_and_no_grep_hits() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    let _global_db = isolate_global_db(&cg);
    let backend = SkillJsonBackend::new(json!({"skills": []}));
    let retrieval = EmptyAutomationSessionRetrieval::new();
    let config = enabled_skill_writer_config();

    let run = run_skill_writer_with_backend_and_retrieval(
        &cg,
        &config,
        &backend,
        &retrieval,
        SkillWriterAutomationOptions {
            include_recent_sessions: false,
            profile_root: Some(profile_root),
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("no_skill_writer_evidence")
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_host_modes_do_not_select_alternate_lcm_storage() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    let project_db = project_session_runtime(&cg).await;
    seed_session_message_in_db(
        &project_db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "project-skill-writer-1",
            message_id: "project-skill-writer-1-message-001",
            role: "assistant",
            timestamp: 1_715_100_005,
            text: "Active project skill writer evidence should draft reusable workflow guidance.",
            source: Some("project_lcm"),
        },
    )
    .await;
    let _global_db = isolate_global_db(&cg);

    let backend = SkillJsonBackend::new(json!({"skills": []}));
    for (host_mode, query) in [
        (
            AutomationHostMode::Standalone,
            "active project skill writer evidence",
        ),
        (
            AutomationHostMode::DelegatedHost,
            "project skill writer evidence",
        ),
    ] {
        let mut config = enabled_skill_writer_config();
        config.host_mode = host_mode;
        let run = run_skill_writer_with_backend(
            &cg,
            &config,
            &backend,
            SkillWriterAutomationOptions {
                provider: "cursor".to_string(),
                query: query.to_string(),
                profile_root: Some(profile_root.clone()),
                ..SkillWriterAutomationOptions::default()
            },
        )
        .await
        .unwrap();
        if host_mode == AutomationHostMode::Standalone {
            assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
        } else {
            assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
            assert_eq!(
                run.ledger_record.error.as_deref(),
                Some("delegated_host_mode")
            );
        }
    }

    assert_eq!(backend.calls(), 1);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_runner_creates_pending_skill_drafts_for_approval() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    let backend = SkillJsonBackend::new(json!({
        "skills": [
            {
                "id": "automation-run-review",
                "title": "Automation run review",
                "summary": "Review self-improvement automation run ledgers and approval gates.",
                "category": "workflow",
                "targets": ["codex", "opencode"],
                "body_markdown": "Use when reviewing TraceDecay self-improvement runs. Check evidence, rejected ops, and pending approval state before applying changes.",
                "support_files": [
                    {
                        "path": "references/checklist.md",
                        "text": "- Check ledger counts\n- Check pending approval state\n"
                    }
                ],
                "reason": "Session evidence repeats automation workflow outcome review."
            },
            {
                "id": "automation-run-review",
                "title": "Duplicate",
                "summary": "Duplicate id should be rejected.",
                "category": "workflow",
                "body_markdown": "Duplicate body."
            },
            {
                "id": "bad/skill",
                "title": "Unsafe",
                "summary": "Unsafe id should be rejected.",
                "category": "workflow",
                "body_markdown": "Unsafe body."
            }
        ]
    }));
    let config = enabled_skill_writer_config();

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        manual_skill_writer_options(&profile_root),
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.task, AgentTaskKind::SkillWriter);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 2);
    assert_eq!(
        run.report["created_skills"][0]["metadata"]["id"],
        json!("automation-run-review")
    );
    assert_eq!(
        run.report["created_skills"][0]["metadata"]["state"],
        json!("pending_approval")
    );
    assert_eq!(
        run.report["created_skills"][0]["proposal_action"],
        json!("create")
    );
    assert_eq!(run.report["created_skills"][0]["action"], json!("create"));
    assert_eq!(
        run.report["created_skills"][0]["proposal_reason"],
        json!("Session evidence repeats automation workflow outcome review.")
    );
    assert_eq!(
        run.report["created_skills"][0]["reason"],
        json!("Session evidence repeats automation workflow outcome review.")
    );
    assert_eq!(
        run.report["created_skills"][0]["approval_status"],
        json!("pending_approval")
    );
    assert!(
        run.report["created_skills"][0]["target_checksum"]
            .as_str()
            .is_some_and(|checksum| checksum.starts_with("sha256:"))
    );
    assert_eq!(
        run.report["created_skills"][0]["metadata"]["targets"],
        json!(["codex", "opencode"])
    );
    let artifact_kinds: Vec<&str> = run
        .ledger_record
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect();
    assert_eq!(
        artifact_kinds,
        vec![
            "traces",
            "feedback",
            "generated_evals",
            "validation_gate",
            "optimizer_diagnosis",
            "codex_handoff"
        ]
    );
    let eval_payload = read_artifact(&cg, &run.run_id, &run.ledger_record, "generated_evals").await;
    assert_eq!(eval_payload["task"], json!("skill_writer"));
    assert_eq!(eval_payload["summary"]["eval_count"], json!(3));
    assert!(
        eval_payload["eval_definitions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["eval_id"] == json!("skill_writer:accepted:0")
                && entry["harness"]["commands"][0]
                    == json!("cargo test --test automation_runner_test skill_writer"))
    );
    assert_eq!(
        eval_payload["runner"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test skill_writer_runner_creates_pending_skill_drafts_for_approval -- --nocapture"
        )
    );
    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(handoff_payload["task"], json!("skill_writer"));
    assert_eq!(
        handoff_payload["next_actions"][0],
        json!("review managed skill drafts or auto-enabled changes")
    );
    assert_eq!(
        handoff_payload["eval_replay"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test skill_writer_runner_creates_pending_skill_drafts_for_approval -- --nocapture"
        )
    );

    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "automation-run-review",
    )
    .await
    .unwrap();
    assert_eq!(
        skill.metadata.state,
        tracedecay::automation::managed_skills::ManagedSkillState::PendingApproval
    );
    assert_eq!(
        skill.metadata.provenance.source,
        tracedecay::automation::managed_skills::ManagedSkillSource::AutomationRun
    );
    assert_eq!(
        skill.metadata.provenance.run_id.as_deref(),
        Some(run.run_id.as_str())
    );
    assert_eq!(
        skill.metadata.targets,
        vec![
            tracedecay::automation::managed_skills::SkillInstallTarget::Codex,
            tracedecay::automation::managed_skills::SkillInstallTarget::OpenCode,
        ]
    );
    assert!(
        profile_root
            .join("agent_managed/skills/automation-run-review/references/checklist.md")
            .is_file()
    );

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
    assert_eq!(records[0].accepted_count, 1);
    assert_eq!(records[0].rejected_count, 2);
    assert_eq!(
        records[0].applied_ops.as_ref().unwrap()["created_skills"][0]["action"],
        json!("create")
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_evidence_imports_project_skill_usage_analytics_before_summarizing() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    seed_search_underuse_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "automation-run-review".to_string(),
            title: "Automation run review".to_string(),
            summary: "Review self-improvement automation runs.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: "Check the run ledger before approving changes.".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::UserDraft,
                actor: "test".to_string(),
                run_id: None,
            },
        },
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    let global_db = project_session_runtime(&cg).await;
    global_db
        .append_profile_analytics_event_for_test(&tracedecay::global_db::AnalyticsEventInsert {
            provider: "codex".to_string(),
            project_id: HostAdmissionTestRuntimeV1::canonical_project_key(cg.project_root()),
            session_id: Some("skill-writer-analytics".to_string()),
            timestamp: 1_715_000_111,
            event_kind: "mcp_tool_call".to_string(),
            hook_name: None,
            tool_name: Some("tracedecay_skill_view".to_string()),
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: None,
            outcome: Some("success".to_string()),
            metadata_json: Some(
                json!({
                    "function": {
                        "name": "tracedecay_skill_view",
                        "arguments": { "id": "automation-run-review" }
                    }
                })
                .to_string(),
            ),
        })
        .await
        .unwrap();
    let backend = InspectSkillWriterUsageBackend;
    let config = enabled_skill_writer_config();

    let run = run_skill_writer_with_backend_and_retrieval(
        &cg,
        &config,
        &backend,
        &FixtureAutomationSessionRetrieval::new(&cg),
        manual_skill_writer_options(&profile_root),
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert!(
        run.report["skill_improvement_recommendations"]
            .as_array()
            .is_some_and(
                |recommendations| recommendations.iter().any(|recommendation| {
                    recommendation["id"] == "underused_tool_family:code_search"
                        && recommendation["source"] == "session_tool_usage"
                })
            )
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_evidence_includes_underused_tool_family_summary() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    seed_search_underuse_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    let backend = InspectSkillWriterUnderusedBackend;
    let config = enabled_skill_writer_config();

    let run = run_skill_writer_with_backend_and_retrieval(
        &cg,
        &config,
        &backend,
        &FixtureAutomationSessionRetrieval::new(&cg),
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "automation".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_runner_auto_enables_when_config_explicitly_allows() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "automation-run-review".to_string(),
            title: "Automation run review".to_string(),
            summary: "Review self-improvement automation runs.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: "Check the run ledger before approving changes.".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::AutomationRun,
                actor: "tracedecay".to_string(),
                run_id: Some("seed-run".to_string()),
            },
        },
    )
    .await
    .unwrap();
    let active = approve_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    let base_checksum = active.metadata.checksum.clone();
    let backend = SkillJsonBackend::with_activation_policy(
        json!({
            "skills": [
                {
                    "id": "scheduler-review",
                    "title": "Scheduler review",
                    "summary": "Review scheduler decisions before enabling automation.",
                    "category": "workflow",
                    "body_markdown": "Check interval gates, cooldowns, locks, and run ledgers before changing schedules.",
                    "reason": "Session evidence repeats scheduler review."
                },
                {
                    "action": "update",
                    "id": "automation-run-review",
                    "base_checksum": base_checksum,
                    "summary": "Review self-improvement automation runs and activation policy.",
                    "body_markdown": "Check the run ledger, activation policy, and approval state before applying changes.",
                    "reason": "Session evidence repeats automation workflow outcome review."
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
    assert_eq!(run.ledger_record.accepted_count, 2);
    assert_eq!(run.ledger_record.rejected_count, 0);
    assert_eq!(run.report["status"], json!("auto_enabled"));
    assert_eq!(
        run.report["activation_policy"],
        json!("auto_enable_after_validation")
    );
    assert_eq!(
        run.report["created_skills"][0]["metadata"]["state"],
        json!("active")
    );
    assert_eq!(
        run.report["updated_skills"][0]["metadata"]["state"],
        json!("active")
    );
    assert_eq!(
        run.report["created_skills"][0]["approval_status"],
        json!("auto_enabled")
    );
    assert_eq!(
        run.report["updated_skills"][0]["proposal_action"],
        json!("update")
    );
    assert_eq!(run.report["updated_skills"][0]["action"], json!("update"));
    assert_eq!(
        run.report["updated_skills"][0]["approval_status"],
        json!("auto_enabled")
    );
    assert_eq!(
        run.report["updated_skills"][0]["base_checksum"],
        json!(base_checksum)
    );

    let created = load_managed_skill(&profile_root, "scheduler-review")
        .await
        .unwrap();
    let updated = load_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    assert_eq!(created.metadata.state, ManagedSkillState::Active);
    assert_eq!(updated.metadata.state, ManagedSkillState::Active);
    assert_eq!(
        updated.metadata.summary,
        "Review self-improvement automation runs and activation policy."
    );
    assert!(updated.pending_update.is_none());
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_runner_updates_existing_skills_with_checksum_precondition() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "automation-run-review".to_string(),
            title: "Automation run review".to_string(),
            summary: "Review self-improvement automation runs.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: "Check the run ledger before approving changes.".to_string(),
            support_files: vec![
                ManagedSupportFile::new("references/old.md", b"old checklist".to_vec()).unwrap(),
            ],
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::AutomationRun,
                actor: "tracedecay".to_string(),
                run_id: Some("seed-run".to_string()),
            },
        },
    )
    .await
    .unwrap();
    let active = approve_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    let base_checksum = active.metadata.checksum.clone();
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "user-owned-review".to_string(),
            title: "User-owned review".to_string(),
            summary: "User-authored workflow.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: "Keep this user-authored content unchanged.".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::UserDraft,
                actor: "test-user".to_string(),
                run_id: None,
            },
        },
    )
    .await
    .unwrap();
    let user_owned = approve_managed_skill(&profile_root, "user-owned-review")
        .await
        .unwrap();
    let backend = SkillJsonBackend::new(json!({
        "skills": [
            {
                "action": "update",
                "id": "automation-run-review",
                "base_checksum": base_checksum.clone(),
                "summary": "Review self-improvement automation runs.",
                "reason": "No-op updates should not be counted as accepted."
            },
            {
                "action": "update",
                "id": "automation-run-review",
                "base_checksum": base_checksum.clone(),
                "summary": "Review automation runs, rejected proposals, and approval gates.",
                "targets": ["claude", "kimi"],
                "body_markdown": "Check the run ledger, rejected proposals, and pending approval state before applying changes.",
                "support_files": [
                    {
                        "path": "references/checklist.md",
                        "text": "- Check ledger counts\n- Check rejected proposals\n"
                    }
                ],
                "reason": "Session evidence repeats automation workflow outcome review."
            },
            {
                "action": "patch",
                "id": "automation-run-review",
                "base_checksum": "sha256:stale",
                "summary": "Stale patch should be rejected."
            },
            {
                "action": "update",
                "id": "missing-skill",
                "base_checksum": "sha256:missing",
                "summary": "Unknown update should be rejected."
            },
            {
                "action": "update",
                "id": "user-owned-review",
                "base_checksum": user_owned.metadata.checksum,
                "summary": "Automation must not edit this user-owned skill."
            }
        ]
    }));
    let config = enabled_skill_writer_config();

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
    assert_eq!(run.ledger_record.rejected_count, 4);
    assert_eq!(run.report["created_skills"], json!([]));
    assert_eq!(
        run.report["updated_skills"][0]["metadata"]["id"],
        json!("automation-run-review")
    );
    assert_eq!(
        run.report["updated_skills"][0]["metadata"]["state"],
        json!("pending_approval")
    );
    assert_eq!(
        run.report["updated_skills"][0]["proposal_action"],
        json!("update")
    );
    assert_eq!(run.report["updated_skills"][0]["action"], json!("update"));
    assert_eq!(
        run.report["updated_skills"][0]["proposal_reason"],
        json!("Session evidence repeats automation workflow outcome review.")
    );
    assert_eq!(
        run.report["updated_skills"][0]["reason"],
        json!("Session evidence repeats automation workflow outcome review.")
    );
    assert_eq!(
        run.report["updated_skills"][0]["approval_status"],
        json!("staged_update")
    );
    assert_eq!(
        run.report["updated_skills"][0]["base_checksum"],
        json!(base_checksum)
    );
    assert_eq!(
        run.report["updated_skills"][0]["metadata"]["targets"],
        json!(["claude", "kimi"])
    );
    assert!(
        run.report["updated_skills"][0]["target_checksum"]
            .as_str()
            .is_some_and(|checksum| checksum.starts_with("sha256:"))
    );

    let skill = load_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    assert_eq!(skill.metadata.state, ManagedSkillState::Active);
    assert_eq!(
        skill.metadata.summary,
        "Review self-improvement automation runs."
    );
    assert_eq!(skill.metadata.checksum, active.metadata.checksum);
    let pending = skill.pending_update.as_ref().unwrap();
    assert_eq!(pending.metadata.state, ManagedSkillState::PendingApproval);
    assert_eq!(
        pending.metadata.summary,
        "Review automation runs, rejected proposals, and approval gates."
    );
    assert_eq!(
        pending.metadata.targets,
        vec![
            tracedecay::automation::managed_skills::SkillInstallTarget::Claude,
            tracedecay::automation::managed_skills::SkillInstallTarget::Kimi,
        ]
    );
    assert_ne!(pending.metadata.checksum, active.metadata.checksum);
    let skill_dir = profile_root.join("agent_managed/skills/automation-run-review");
    assert!(skill_dir.join("references/old.md").is_file());
    assert!(!skill_dir.join("references/checklist.md").exists());

    let approved = approve_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    assert_eq!(approved.metadata.state, ManagedSkillState::Active);
    assert_eq!(
        approved.metadata.summary,
        "Review automation runs, rejected proposals, and approval gates."
    );
    assert!(approved.pending_update.is_none());
    assert!(!skill_dir.join("references/old.md").exists());
    assert!(skill_dir.join("references/checklist.md").is_file());

    let user_owned = load_managed_skill(&profile_root, "user-owned-review")
        .await
        .unwrap();
    assert_eq!(user_owned.metadata.summary, "User-authored workflow.");
    assert!(user_owned.pending_update.is_none());

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].accepted_count, 1);
    assert_eq!(records[0].rejected_count, 4);
    assert_eq!(
        records[0].proposed_ops.as_ref().unwrap()["updated_skills"][0]["metadata"]["id"],
        json!("automation-run-review")
    );
    assert_eq!(
        records[0].applied_ops.as_ref().unwrap()["updated_skills"][0]["approval_status"],
        json!("staged_update")
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_runner_ledgers_malformed_backend_output() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    let backend = SkillTextBackend::new("not json");
    let config = enabled_skill_writer_config();

    let err = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        manual_skill_writer_options(&profile_root),
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string().contains("expected ident") || err.to_string().contains("expected value"),
        "unexpected error: {err}"
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].schema_version, 2);
    assert_eq!(records[0].task, AgentTaskKind::SkillWriter);
    assert_eq!(records[0].task_key.as_deref(), Some("skill_writer"));
    assert_eq!(
        records[0].prompt_version.as_deref(),
        Some("skill_writer:v2")
    );
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert_eq!(records[0].reviewed_count, 0);
    assert_eq!(records[0].skipped_count, 0);
    assert_eq!(records[0].model.as_deref(), Some("fixture-model"));
    assert!(records[0].evidence_hash.is_some());
    assert!(records[0].error.as_deref().is_some_and(|error| {
        error.contains("expected ident") || error.contains("expected value")
    }));
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_runner_ledgers_missing_skills_array() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    let output = json!({"summary": "no skills"});
    let backend = SkillJsonBackend::new(output.clone());
    let config = enabled_skill_writer_config();

    let err = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        manual_skill_writer_options(&profile_root),
    )
    .await
    .unwrap_err();

    assert_eq!(backend.calls(), 1);
    assert!(
        err.to_string()
            .contains("skill writer output must include a skills array")
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].schema_version, 2);
    assert_eq!(records[0].task, AgentTaskKind::SkillWriter);
    assert_eq!(records[0].task_key.as_deref(), Some("skill_writer"));
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert_eq!(records[0].model.as_deref(), Some("fixture-model"));
    assert!(records[0].evidence_hash.is_some());
    assert!(records[0].input_hash.is_some());
    assert_eq!(records[0].proposed_ops.as_ref(), Some(&output));
    assert!(
        records[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("skill writer output must include a skills array"))
    );
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_writer_runner_records_noop_fallback_when_backend_run_task_fails() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    let backend = FailingBackend::new(AgentTaskKind::SkillWriter);
    let config = AutomationConfig {
        timeout_secs: 1,
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

    // The backend failure is transient, but this test pins the noop-fallback
    // record, not retry semantics (covered by backend.rs retry tests) —
    // timeout_secs: 1 short-circuits the backoff so the test stays fast.
    assert_eq!(backend.calls(), 1);
    assert_noop_fallback_record(
        &run.ledger_record,
        AgentTaskKind::SkillWriter,
        "skill_writer",
        json!({ "skills": [] }),
    );
    assert!(
        run.ledger_record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("executable"))
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_noop_fallback_record(
        &records[0],
        AgentTaskKind::SkillWriter,
        "skill_writer",
        json!({ "skills": [] }),
    );
}

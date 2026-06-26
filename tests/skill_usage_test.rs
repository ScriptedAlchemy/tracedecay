use tracedecay::automation::managed_skills::{
    create_managed_skill_draft, default_managed_skill_targets, ManagedSkillDraft,
    ManagedSkillProvenance, ManagedSkillSource,
};
use tracedecay::automation::skill_usage::{
    ingest_analytics_events, record_skill_usage, record_skill_usage_event,
    skill_improvement_recommendations, stale_skill_recommendations, summarize_skill_usage,
    SkillUsageAction, SkillUsageEvent,
};
use tracedecay::global_db::AnalyticsEventRecord;

fn draft(id: &str, source: ManagedSkillSource) -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: id.to_string(),
        title: format!("{id} skill"),
        summary: "Managed skill usage test fixture.".to_string(),
        category: "maintenance".to_string(),
        targets: default_managed_skill_targets(),
        body_markdown: "Use this skill when maintaining automation.".to_string(),
        support_files: Vec::new(),
        provenance: ManagedSkillProvenance {
            source,
            actor: match source {
                ManagedSkillSource::UserDraft => "user".to_string(),
                _ => "tracedecay".to_string(),
            },
            run_id: Some("run_usage".to_string()),
        },
    }
}

#[tokio::test]
async fn skill_usage_ledger_records_views_uses_patches_and_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let skill = create_managed_skill_draft(
        &profile_root,
        draft("repo-hygiene", ManagedSkillSource::AutomationRun),
    )
    .await
    .unwrap();

    for (action, timestamp, target) in [
        (SkillUsageAction::View, 100, Some("Cursor")),
        (SkillUsageAction::Use, 150, Some("codex")),
        (SkillUsageAction::Patch, 125, None),
    ] {
        record_skill_usage_event(
            &profile_root,
            SkillUsageEvent {
                skill_name: "repo-hygiene".to_string(),
                action,
                timestamp,
                target: target.map(str::to_string),
            },
            Some(&skill),
        )
        .await
        .unwrap();
    }

    let summaries = summarize_skill_usage(&profile_root, std::slice::from_ref(&skill))
        .await
        .unwrap();
    let summary = &summaries[0];
    assert_eq!(summary.skill_id, "repo-hygiene");
    assert_eq!(summary.view_count, 1);
    assert_eq!(summary.use_count, 1);
    assert_eq!(summary.patch_count, 1);
    assert_eq!(summary.last_activity_at, 150);
    assert_eq!(summary.targets, vec!["codex", "cursor"]);
    assert_eq!(summary.created_by.as_deref(), Some("tracedecay"));
    assert_eq!(
        summary.provenance_source,
        Some(ManagedSkillSource::AutomationRun)
    );
}

#[tokio::test]
async fn analytics_ingest_normalizes_skill_usage_events() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");

    let events = vec![AnalyticsEventRecord {
        id: 7,
        provider: "codex".to_string(),
        project_id: "project".to_string(),
        session_id: Some("session".to_string()),
        timestamp: 200,
        event_kind: "tool".to_string(),
        hook_name: None,
        tool_name: Some("skill_view".to_string()),
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: None,
        metadata_json: Some(
            r#"{"function":{"name":"skill_view","arguments":{"name":"skill:repo-hygiene"}}}"#
                .to_string(),
        ),
    }];

    let touched = ingest_analytics_events(&profile_root, &events)
        .await
        .unwrap();
    assert_eq!(touched.len(), 1);
    assert_eq!(touched[0].skill_id, "repo-hygiene");
    assert_eq!(touched[0].view_count, 1);
    assert_eq!(touched[0].targets, vec!["codex"]);
}

#[tokio::test]
async fn analytics_ingest_counts_tracedecay_skill_view_id_as_view() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");

    let events = vec![AnalyticsEventRecord {
        id: 8,
        provider: "codex".to_string(),
        project_id: "project".to_string(),
        session_id: Some("session".to_string()),
        timestamp: 240,
        event_kind: "mcp_tool_call".to_string(),
        hook_name: None,
        tool_name: Some("tracedecay_skill_view".to_string()),
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: None,
        metadata_json: Some(
            r#"{"function":{"name":"tracedecay_skill_view","arguments":{"id":"repo-hygiene"}}}"#
                .to_string(),
        ),
    }];

    let touched = ingest_analytics_events(&profile_root, &events)
        .await
        .unwrap();
    assert_eq!(touched.len(), 1);
    assert_eq!(touched[0].skill_id, "repo-hygiene");
    assert_eq!(touched[0].view_count, 1);
    assert_eq!(touched[0].use_count, 0);
    assert_eq!(touched[0].last_viewed_at, Some(240));
    assert_eq!(touched[0].targets, vec!["codex"]);
}

#[tokio::test]
async fn analytics_ingest_skips_failed_tracedecay_skill_view_rows() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");

    let events = vec![AnalyticsEventRecord {
        id: 10,
        provider: "codex".to_string(),
        project_id: "project".to_string(),
        session_id: Some("session".to_string()),
        timestamp: 245,
        event_kind: "mcp_tool_call".to_string(),
        hook_name: None,
        tool_name: Some("tracedecay_skill_view".to_string()),
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: Some("error".to_string()),
        metadata_json: Some(
            r#"{"request_id":"req-failed","function":{"name":"tracedecay_skill_view","arguments":{"id":"repo-hygiene"}}}"#
                .to_string(),
        ),
    }];

    let touched = ingest_analytics_events(&profile_root, &events)
        .await
        .unwrap();
    assert!(touched.is_empty());
    assert!(
        tracedecay::automation::skill_usage::load_skill_usage_record(&profile_root, "repo-hygiene")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn analytics_ingest_dedupes_tracedecay_skill_view_by_request_id() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");

    let events = vec![
        AnalyticsEventRecord {
            id: 11,
            provider: "codex".to_string(),
            project_id: "project".to_string(),
            session_id: Some("session".to_string()),
            timestamp: 250,
            event_kind: "mcp_tool_call".to_string(),
            hook_name: None,
            tool_name: Some("tracedecay_skill_view".to_string()),
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: None,
            outcome: Some("success".to_string()),
            metadata_json: Some(
                r#"{"request_id":"req-view-1","function":{"name":"tracedecay_skill_view","arguments":{"id":"repo-hygiene"}}}"#
                    .to_string(),
            ),
        },
        AnalyticsEventRecord {
            id: 12,
            provider: "codex".to_string(),
            project_id: "project".to_string(),
            session_id: Some("session".to_string()),
            timestamp: 255,
            event_kind: "mcp_tool_call".to_string(),
            hook_name: None,
            tool_name: Some("tracedecay_skill_view".to_string()),
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: None,
            outcome: Some("success".to_string()),
            metadata_json: Some(
                r#"{"request_id":"req-view-1","function":{"name":"tracedecay_skill_view","arguments":{"id":"repo-hygiene"}}}"#
                    .to_string(),
            ),
        },
    ];

    let touched = ingest_analytics_events(&profile_root, &events)
        .await
        .unwrap();
    assert_eq!(touched.len(), 1);
    assert_eq!(touched[0].view_count, 1);
    assert_eq!(touched[0].last_viewed_at, Some(250));

    let second = ingest_analytics_events(&profile_root, &events)
        .await
        .unwrap();
    assert!(second.is_empty());

    let record =
        tracedecay::automation::skill_usage::load_skill_usage_record(&profile_root, "repo-hygiene")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(record.view_count, 1);
    assert_eq!(record.targets, vec!["codex"]);
}

#[tokio::test]
async fn direct_skill_view_marks_matching_analytics_request_imported() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let skill = create_managed_skill_draft(
        &profile_root,
        draft("repo-hygiene", ManagedSkillSource::AutomationRun),
    )
    .await
    .unwrap();

    record_skill_usage(
        &profile_root,
        &skill,
        SkillUsageAction::View,
        "mcp",
        vec!["mcp".to_string()],
        Some("mcp".to_string()),
        Some(serde_json::json!({
            "imported_analytics_event_key": "project:mcp:request:req-view-1:repo-hygiene:View",
        })),
    )
    .await
    .unwrap();

    let touched = ingest_analytics_events(
        &profile_root,
        &[AnalyticsEventRecord {
            id: 13,
            provider: "mcp".to_string(),
            project_id: "project".to_string(),
            session_id: Some("session".to_string()),
            timestamp: 300,
            event_kind: "mcp_tool_call".to_string(),
            hook_name: None,
            tool_name: Some("tracedecay_skill_view".to_string()),
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: None,
            outcome: Some("success".to_string()),
            metadata_json: Some(
                r#"{"request_id":"req-view-1","function":{"name":"tracedecay_skill_view","arguments":{"id":"repo-hygiene"}}}"#
                    .to_string(),
            ),
        }],
    )
    .await
    .unwrap();

    assert!(touched.is_empty());
    let record =
        tracedecay::automation::skill_usage::load_skill_usage_record(&profile_root, "repo-hygiene")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(record.view_count, 1);
    assert_eq!(record.targets, vec!["mcp"]);
}

#[tokio::test]
async fn analytics_ingest_is_idempotent_and_accepts_bare_skill_name_rows() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");

    let events = vec![AnalyticsEventRecord {
        id: 9,
        provider: "cursor".to_string(),
        project_id: "project".to_string(),
        session_id: Some("session".to_string()),
        timestamp: 260,
        event_kind: "skill".to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: Some("repo-hygiene".to_string()),
        hint_category: None,
        hint_id: None,
        outcome: None,
        metadata_json: None,
    }];

    let first = ingest_analytics_events(&profile_root, &events)
        .await
        .unwrap();
    let second = ingest_analytics_events(&profile_root, &events)
        .await
        .unwrap();

    assert_eq!(first.len(), 1);
    assert!(second.is_empty());
    assert_eq!(first[0].skill_id, "repo-hygiene");
    assert_eq!(first[0].use_count, 1);

    let record =
        tracedecay::automation::skill_usage::load_skill_usage_record(&profile_root, "repo-hygiene")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(record.use_count, 1);
    assert_eq!(record.targets, vec!["cursor"]);
}

#[tokio::test]
async fn stale_scoring_explains_archive_candidates_and_exclusions() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let stale_skill = create_managed_skill_draft(
        &profile_root,
        draft("stale-skill", ManagedSkillSource::AutomationRun),
    )
    .await
    .unwrap();
    let pinned_skill = create_managed_skill_draft(
        &profile_root,
        draft("pinned-skill", ManagedSkillSource::AutomationRun),
    )
    .await
    .unwrap();
    let mut pinned = pinned_skill.clone();
    pinned.set_pinned(true);
    tracedecay::automation::managed_skills::save_managed_skill(&profile_root, &pinned)
        .await
        .unwrap();
    let user_skill = create_managed_skill_draft(
        &profile_root,
        draft("user-skill", ManagedSkillSource::UserDraft),
    )
    .await
    .unwrap();

    record_skill_usage_event(
        &profile_root,
        SkillUsageEvent {
            skill_name: "stale-skill".to_string(),
            action: SkillUsageAction::View,
            timestamp: 100,
            target: Some("cursor".to_string()),
        },
        Some(&stale_skill),
    )
    .await
    .unwrap();

    let summaries = summarize_skill_usage(&profile_root, &[stale_skill, pinned, user_skill])
        .await
        .unwrap();
    let recommendations = stale_skill_recommendations(&summaries, 10_000, 1_000);

    let stale = recommendations
        .iter()
        .find(|recommendation| recommendation.skill_id == "stale-skill")
        .unwrap();
    assert!(stale.stale);
    assert_eq!(stale.recommendation, "archive_review");
    assert!(stale.reason.contains("no recorded uses or patches"));
    assert!(stale
        .evidence
        .iter()
        .any(|entry| entry == "provenance_source=automation_run"));

    let pinned = recommendations
        .iter()
        .find(|recommendation| recommendation.skill_id == "pinned-skill")
        .unwrap();
    assert!(!pinned.stale);
    assert!(pinned.reason.contains("pinned"));

    let user = recommendations
        .iter()
        .find(|recommendation| recommendation.skill_id == "user-skill")
        .unwrap();
    assert!(!user.stale);
    assert!(user.reason.contains("user-authored"));
}

#[tokio::test]
async fn repeated_skill_patches_recommend_improvement_review() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let skill = create_managed_skill_draft(
        &profile_root,
        draft("review-loop", ManagedSkillSource::AutomationRun),
    )
    .await
    .unwrap();

    for (action, timestamp) in [
        (SkillUsageAction::Use, 100),
        (SkillUsageAction::Patch, 125),
        (SkillUsageAction::Patch, 175),
    ] {
        record_skill_usage_event(
            &profile_root,
            SkillUsageEvent {
                skill_name: "review-loop".to_string(),
                action,
                timestamp,
                target: Some("codex".to_string()),
            },
            Some(&skill),
        )
        .await
        .unwrap();
    }

    let summaries = summarize_skill_usage(&profile_root, std::slice::from_ref(&skill))
        .await
        .unwrap();
    let recommendations = skill_improvement_recommendations(&summaries);
    let recommendation = recommendations
        .iter()
        .find(|recommendation| recommendation.skill_id == "review-loop")
        .unwrap();

    assert!(recommendation.improvement);
    assert_eq!(recommendation.recommendation, "patch_review");
    assert_eq!(recommendation.priority, "medium");
    assert!(recommendation.reason.contains("repeated patches"));
    assert!(recommendation
        .evidence
        .iter()
        .any(|entry| entry == "patches=2"));
}

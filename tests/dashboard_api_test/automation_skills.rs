use crate::dashboard_api_support::*;

#[test]
fn managed_skills_are_dashboard_controllable_and_persistent() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();
        let base_url = &fixture.base_url;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["features"]["managed_skills"], true);

        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, empty) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(empty["count"], 0);
        assert_eq!(empty["skills"].as_array().map(Vec::len), Some(0));

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repo Hygiene",
            "summary": "Keep repository maintenance tasks consistent.",
            "category": "workflow",
            "body_markdown": "Use this when cleaning generated changes.",
            "support_files": [
                {
                    "path": "references/checklist.md",
                    "bytes": [99, 104, 101, 99, 107]
                }
            ],
            "provenance": {
                "source": "automation_run",
                "actor": "dashboard-test",
                "run_id": "run-dashboard-1"
            }
        });
        let (status, created) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/skills/draft"),
            &draft,
        );
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["id"], "repo-hygiene");
        assert_eq!(created["skill"]["metadata"]["state"], "pending_approval");
        assert!(created["skill"]["metadata"]["created_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert!(created["skill"]["metadata"]["updated_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert_eq!(created["usage_summary"]["view_count"], 0);
        assert_eq!(
            created["skill"]["metadata"]["provenance"]["run_id"],
            "run-dashboard-1"
        );
        let profile_root = tracedecay::storage::default_profile_root().unwrap();
        let skill = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        tracedecay::automation::skill_usage::record_skill_usage(
            &profile_root,
            &skill,
            tracedecay::automation::skill_usage::SkillUsageAction::Use,
            "dashboard-test",
            vec!["cursor".to_string(), "codex".to_string()],
            Some("cursor".to_string()),
            None,
        )
        .await
        .unwrap();
        let global_db = GlobalDb::open()
            .await
            .expect("dashboard fixture global db opens");
        global_db
            .append_analytics_event(&tracedecay::global_db::AnalyticsEventInsert {
                provider: "mcp".to_string(),
                project_id: GlobalDb::canonical_project_key(&fixture.project_root),
                session_id: Some("dashboard-skill-session".to_string()),
                timestamp: tracedecay::tracedecay::current_timestamp(),
                event_kind: "mcp_tool_call".to_string(),
                hook_name: None,
                tool_name: Some("tracedecay_skill_view".to_string()),
                tool_category: None,
                skill_name: None,
                hint_category: None,
                hint_id: None,
                outcome: Some("success".to_string()),
                metadata_json: Some(
                    serde_json::json!({
                        "function": {
                            "name": "tracedecay_skill_view",
                            "arguments": { "id": "repo-hygiene" }
                        }
                    })
                    .to_string(),
                ),
            })
            .await
            .unwrap();

        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");
        assert_eq!(listed["usage_summaries"][0]["view_count"], 1);
        assert_eq!(listed["usage_summaries"][0]["use_count"], 1);
        assert_eq!(
            listed["usage_summaries"][0]["targets"],
            serde_json::json!(["codex", "cursor", "mcp"])
        );
        assert_eq!(listed["stale_recommendations"][0]["skill_id"], "repo-hygiene");
        assert_eq!(listed["stale_recommendations"][0]["stale"], false);
        assert_eq!(listed["stale_recommendations"][0]["recommendation"], "keep");
        assert_eq!(
            listed["improvement_recommendations"][0]["skill_id"],
            "repo-hygiene"
        );
        assert_eq!(
            listed["improvement_recommendations"][0]["recommendation"],
            "none"
        );

        let skill_url = format!("{base_url}/api/automation/skills/repo-hygiene");
        let (status, viewed) = get_json(&agent, &skill_url);
        assert_eq!(status, 200);
        assert_eq!(
            viewed["skill"]["body_markdown"],
            "Use this when cleaning generated changes."
        );
        assert_eq!(viewed["usage_summary"]["use_count"], 1);
        assert_eq!(viewed["stale_recommendation"]["recommendation"], "keep");
        assert_eq!(viewed["improvement_recommendation"]["recommendation"], "none");

        let (status, approved) = post_json(&agent, &format!("{skill_url}/approve"));
        assert_eq!(status, 200);
        assert_eq!(approved["skill"]["metadata"]["state"], "active");
        assert_eq!(
            approved["skill"]["metadata"]["created_at"],
            created["skill"]["metadata"]["created_at"]
        );
        assert!(
            approved["skill"]["metadata"]["updated_at"]
                .as_i64()
                .unwrap_or_default()
                >= created["skill"]["metadata"]["updated_at"]
                    .as_i64()
                    .unwrap_or_default()
        );

        let duplicate = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Overwrite attempt",
            "summary": "This should not replace the approved skill.",
            "category": "workflow",
            "body_markdown": "Duplicate drafts must not bypass PATCH staging.",
            "support_files": [
                {
                    "path": "templates/overwrite.md",
                    "bytes": [111, 118, 101, 114, 119, 114, 105, 116, 101]
                }
            ]
        });
        let (status, conflict) = post_json_body(&agent, &skills_url, &duplicate);
        assert_eq!(status, 409);
        assert!(conflict["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("already exists")));
        let persisted_after_duplicate =
            tracedecay::automation::managed_skills::load_managed_skill(
                &profile_root,
                "repo-hygiene",
            )
            .await
            .unwrap();
        assert_eq!(
            persisted_after_duplicate.body_markdown,
            "Use this when cleaning generated changes."
        );
        assert!(profile_root
            .join("agent_managed/skills/repo-hygiene/references/checklist.md")
            .is_file());
        assert!(!profile_root
            .join("agent_managed/skills/repo-hygiene/templates/overwrite.md")
            .exists());

        let (status, missing_checksum) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "summary": "Updated after dashboard review.",
                "body_markdown": "Use this when cleaning generated changes and record focused checks.",
                "pinned": true
            }),
        );
        assert_eq!(status, 400);
        assert!(missing_checksum["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("base_checksum")));

        let (status, patched) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "base_checksum": approved["skill"]["metadata"]["checksum"],
                "summary": "Updated after dashboard review.",
                "body_markdown": "Use this when cleaning generated changes and record focused checks.",
                "pinned": true
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(
            patched["skill"]["metadata"]["summary"],
            "Keep repository maintenance tasks consistent."
        );
        assert_eq!(patched["skill"]["metadata"]["state"], "active");
        assert_eq!(patched["skill"]["metadata"]["pinned"], false);
        assert_eq!(
            patched["skill"]["pending_update"]["metadata"]["summary"],
            "Updated after dashboard review."
        );
        assert_eq!(patched["skill"]["pending_update"]["metadata"]["pinned"], true);
        assert_eq!(
            patched["skill"]["pending_update"]["base_checksum"],
            approved["skill"]["metadata"]["checksum"]
        );
        assert_eq!(
            patched["skill"]["metadata"]["created_at"],
            created["skill"]["metadata"]["created_at"]
        );

        for (action, expected_state) in [
            ("approve", "active"),
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "pending_approval"),
        ] {
            let (status, updated) = post_json(&agent, &format!("{skill_url}/{action}"));
            assert_eq!(status, 200, "{action} should succeed");
            assert_eq!(updated["skill"]["metadata"]["state"], expected_state);
        }

        let persisted = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        assert_eq!(
            persisted.metadata.state,
            tracedecay::automation::managed_skills::ManagedSkillState::PendingApproval
        );
    });
}

#[test]
fn managed_skills_are_dashboard_controllable_with_explicit_approval() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, initial) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(initial["count"], 0);

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repository hygiene",
            "summary": "Keep repository checks focused.",
            "category": "maintenance",
            "body_markdown": "Run focused tests before broad suites.",
            "pinned": true
        });
        let (status, created) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["state"], "pending_approval");
        assert_eq!(created["skill"]["metadata"]["pinned"], true);
        assert_eq!(
            created["skill"]["metadata"]["provenance"]["source"],
            "user_draft"
        );

        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");
        assert_eq!(listed["skills"][0]["metadata"]["state"], "pending_approval");

        let skill_url = format!("{base_url}/api/automation/skills/repo-hygiene");
        let (status, updated) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "summary": "Updated with review evidence.",
                "body_markdown": "Record the narrow command that covers each change."
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(
            updated["skill"]["metadata"]["summary"],
            "Updated with review evidence."
        );
        assert_eq!(updated["skill"]["metadata"]["state"], "pending_approval");

        for (action, expected_state) in [
            ("approve", "active"),
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "pending_approval"),
        ] {
            let (status, payload) = post_json_body(
                &agent,
                &format!("{base_url}/api/automation/skills/repo-hygiene/{action}"),
                &serde_json::json!({}),
            );
            assert_eq!(status, 200, "{action} should succeed: {payload}");
            assert_eq!(payload["skill"]["metadata"]["state"], expected_state);
        }

        let skill_dir = profile_root
            .join("agent_managed")
            .join("skills")
            .join("repo-hygiene");
        assert!(skill_dir.join("skill.json").is_file());
        assert!(skill_dir.join("SKILL.md").is_file());
        server.stop();
    });
}

#[test]
fn managed_skill_dashboard_api_persists_and_updates_lifecycle() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repository hygiene",
            "summary": "Keep repository maintenance guidance current.",
            "category": "maintenance",
            "body_markdown": "Use focused checks before changing generated files.",
            "support_files": [
                {
                    "path": "references/checklist.md",
                    "bytes": [45, 32, 114, 117, 110, 32, 116, 101, 115, 116, 115, 10]
                }
            ],
            "provenance": {
                "source": "user_draft",
                "actor": "dashboard",
                "run_id": null
            }
        });
        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, created) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["state"], "pending_approval");
        assert!(created["skill"]["metadata"]["created_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert!(created["skill"]["metadata"]["updated_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert!(
            profile_root
                .join("agent_managed/skills/repo-hygiene/SKILL.md")
                .is_file(),
            "drafting a managed skill must persist a SKILL.md package"
        );

        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");

        let (status, viewed) = get_json(
            &agent,
            &format!("{base_url}/api/automation/skills/repo-hygiene"),
        );
        assert_eq!(status, 200);
        assert_eq!(viewed["skill"]["metadata"]["id"], "repo-hygiene");

        for (action, expected_state) in [
            ("approve", "active"),
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "pending_approval"),
        ] {
            let (status, response) = post_json(
                &agent,
                &format!("{base_url}/api/automation/skills/repo-hygiene/{action}"),
            );
            assert_eq!(status, 200);
            assert_eq!(response["skill"]["metadata"]["state"], expected_state);
        }
        server.stop();
    });
}

#[test]
fn managed_skill_dashboard_api_controls_staged_updates() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repository hygiene",
            "summary": "Keep repository maintenance guidance current.",
            "category": "maintenance",
            "body_markdown": "Use focused checks before changing generated files.",
            "support_files": [
                {
                    "path": "references/checklist.md",
                    "bytes": [45, 32, 114, 117, 110, 32, 116, 101, 115, 116, 115, 10]
                }
            ],
            "provenance": {
                "source": "user_draft",
                "actor": "dashboard",
                "run_id": null
            }
        });
        let skills_url = format!("{base_url}/api/automation/skills");
        let skill_url = format!("{skills_url}/repo-hygiene");
        let (status, _) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        let (status, _) = post_json(&agent, &format!("{skill_url}/approve"));
        assert_eq!(status, 200);

        let active = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        let base_checksum = active.metadata.checksum.clone();
        tracedecay::automation::managed_skills::stage_managed_skill_update(
            &profile_root,
            "repo-hygiene",
            &base_checksum,
            tracedecay::automation::managed_skills::ManagedSkillUpdate {
                summary: Some("Stage dashboard-visible generated guidance.".to_string()),
                body_markdown: Some(
                    "Review the run ledger before applying generated edits.".to_string(),
                ),
                support_files: Some(vec![
                    tracedecay::automation::managed_skills::ManagedSupportFile::new(
                        "templates/review.md",
                        b"review body".to_vec(),
                    )
                    .unwrap(),
                ]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (status, staged_view) = get_json(&agent, &skill_url);
        assert_eq!(status, 200);
        assert_eq!(staged_view["skill"]["metadata"]["state"], "active");
        assert_eq!(
            staged_view["skill"]["metadata"]["summary"],
            "Keep repository maintenance guidance current."
        );
        assert_eq!(
            staged_view["skill"]["pending_update"]["metadata"]["summary"],
            "Stage dashboard-visible generated guidance."
        );
        let skill_dir = profile_root.join("agent_managed/skills/repo-hygiene");
        assert!(skill_dir.join("references/checklist.md").is_file());
        assert!(!skill_dir.join("templates/review.md").exists());

        let (status, discarded) = post_json(&agent, &format!("{skill_url}/discard-update"));
        assert_eq!(status, 200);
        assert!(discarded["skill"]["pending_update"].is_null());
        assert_eq!(
            discarded["skill"]["metadata"]["summary"],
            "Keep repository maintenance guidance current."
        );

        let active = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        tracedecay::automation::managed_skills::stage_managed_skill_update(
            &profile_root,
            "repo-hygiene",
            &active.metadata.checksum,
            tracedecay::automation::managed_skills::ManagedSkillUpdate {
                summary: Some("Approve dashboard-visible generated guidance.".to_string()),
                body_markdown: Some(
                    "Review the run ledger before applying generated edits.".to_string(),
                ),
                support_files: Some(vec![
                    tracedecay::automation::managed_skills::ManagedSupportFile::new(
                        "templates/review.md",
                        b"review body".to_vec(),
                    )
                    .unwrap(),
                ]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (status, approved) = post_json(&agent, &format!("{skill_url}/approve"));
        assert_eq!(status, 200);
        assert_eq!(approved["skill"]["metadata"]["state"], "active");
        assert_eq!(
            approved["skill"]["metadata"]["summary"],
            "Approve dashboard-visible generated guidance."
        );
        assert!(approved["skill"]["pending_update"].is_null());
        assert!(!skill_dir.join("references/checklist.md").exists());
        assert!(skill_dir.join("templates/review.md").is_file());

        server.stop();
    });
}

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

        let (status, invalid) = post_json_body(
            &agent,
            &skills_url,
            &serde_json::json!({
                "id": "../ambient-skill",
                "title": "Invalid skill",
                "summary": "Must not escape the managed profile root.",
                "category": "workflow",
                "body_markdown": "invalid"
            }),
        );
        assert_eq!(status, 400, "invalid draft must stay typed: {invalid}");
        let (status, missing) = post_json(
            &agent,
            &format!("{skills_url}/missing-skill/disable"),
        );
        assert_eq!(status, 404, "missing target must stay typed: {missing}");

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
        let (status, created) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["id"], "repo-hygiene");
        assert_eq!(created["skill"]["metadata"]["state"], "active");
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
        let profile_root = fixture.host_runtime.profile_root().to_path_buf();
        let skill = tracedecay_agent_hosts::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        tracedecay_agent_hosts::automation::skill_usage::record_skill_usage(
            &profile_root,
            &skill,
            tracedecay_agent_hosts::automation::skill_usage::SkillUsageAction::Use,
            "dashboard-test",
            vec!["cursor".to_string(), "codex".to_string()],
            Some("cursor".to_string()),
            None,
        )
        .await
        .unwrap();
        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");
        assert_eq!(listed["usage_summaries"][0]["view_count"], 0);
        assert_eq!(listed["usage_summaries"][0]["use_count"], 1);
        assert_eq!(
            listed["usage_summaries"][0]["targets"],
            serde_json::json!(["codex", "cursor"])
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
        assert_eq!(viewed["usage_summary"]["view_count"], 0);
        assert_eq!(viewed["stale_recommendation"]["recommendation"], "keep");
        assert_eq!(viewed["improvement_recommendation"]["recommendation"], "none");

        let duplicate = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Overwrite attempt",
            "summary": "This should not replace the active skill.",
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
            tracedecay_agent_hosts::automation::managed_skills::load_managed_skill(
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

        let (status, patched) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "base_checksum": created["skill"]["metadata"]["checksum"],
                "summary": "Updated after dashboard review.",
                "body_markdown": "Use this when cleaning generated changes and record focused checks.",
                "pinned": true
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(
            patched["skill"]["metadata"]["summary"],
            "Updated after dashboard review."
        );
        assert_eq!(patched["skill"]["metadata"]["state"], "active");
        assert_eq!(patched["skill"]["metadata"]["pinned"], true);
        assert!(patched["skill"]["pending_update"].is_null());
        assert_eq!(
            patched["skill"]["metadata"]["created_at"],
            created["skill"]["metadata"]["created_at"]
        );

        for (action, expected_state) in [
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "active"),
        ] {
            let (status, updated) = post_json(&agent, &format!("{skill_url}/{action}"));
            assert_eq!(status, 200, "{action} should succeed");
            assert_eq!(updated["skill"]["metadata"]["state"], expected_state);
        }

        let persisted = tracedecay_agent_hosts::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        assert_eq!(
            persisted.metadata.state,
            tracedecay_agent_hosts::automation::managed_skills::ManagedSkillState::Active
        );
    });
}

#[test]
fn managed_skills_are_dashboard_controllable_with_direct_activation() {
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
        let home = tmp_root.join("home");
        let profile_root = home.join(".tracedecay");
        std::fs::create_dir_all(&home).unwrap();
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
        let _home_guard = EnvVarGuard::set("HOME", &home);
        let _userprofile_guard = EnvVarGuard::set("USERPROFILE", &home);

        let (cg, host_runtime) = setup_project(&project_root).await;
        let managed_skill_profile_root = host_runtime.profile_root().to_path_buf();
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server_with_host_runtime(
            cg,
            host_runtime,
            dashboard::DashboardTestProjectGraphsV1::default(),
            port,
        );
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
        assert_eq!(created["skill"]["metadata"]["state"], "active");
        assert_eq!(created["skill"]["metadata"]["pinned"], true);
        assert_eq!(created["skill"]["metadata"]["provenance"]["source"], "user");

        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");
        assert_eq!(listed["skills"][0]["metadata"]["state"], "active");

        let skill_url = format!("{base_url}/api/automation/skills/repo-hygiene");
        let (status, updated) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "base_checksum": created["skill"]["metadata"]["checksum"],
                "summary": "Updated with review evidence.",
                "body_markdown": "Record the narrow command that covers each change."
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(
            updated["skill"]["metadata"]["summary"],
            "Updated with review evidence."
        );
        assert_eq!(updated["skill"]["metadata"]["state"], "active");

        // Detected global and project-local Claude Code installs must receive
        // direct lifecycle changes without waiting for the next
        // `tracedecay install` / `update-plugin`.
        let claude_md = home.join(".claude/CLAUDE.md");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(&claude_md, "# Claude rules\n").unwrap();
        // A global Claude install is detected through the deployed plugin
        // marketplace manifest (the `tracedecay install` signal), not through
        // a raw mcpServers entry.
        let marketplace_manifest =
            home.join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/marketplace.json");
        std::fs::create_dir_all(marketplace_manifest.parent().unwrap()).unwrap();
        std::fs::write(
            &marketplace_manifest,
            r#"{"name":"tracedecay","owner":{"name":"tracedecay"},"plugins":[]}"#,
        )
        .unwrap();
        let local_claude_md = project_root.join(".claude/CLAUDE.md");
        std::fs::create_dir_all(project_root.join(".claude")).unwrap();
        std::fs::write(&local_claude_md, "# Local Claude rules\n").unwrap();
        std::fs::write(
            project_root.join(".mcp.json"),
            r#"{"mcpServers":{"tracedecay":{"command":"tracedecay","args":["serve"]}}}"#,
        )
        .unwrap();

        for (action, expected_state, expect_deployed) in [
            ("disable", "disabled", false),
            ("archive", "archived", false),
            ("restore", "active", true),
        ] {
            let (status, payload) = post_json_body(
                &agent,
                &format!("{base_url}/api/automation/skills/repo-hygiene/{action}"),
                &serde_json::json!({}),
            );
            assert_eq!(status, 200, "{action} should succeed: {payload}");
            assert_eq!(payload["skill"]["metadata"]["state"], expected_state);

            let exports = payload["deployment"]["exports"]
                .as_array()
                .unwrap_or_else(|| panic!("{action} should report skill exports: {payload}"));
            let claude_report = exports
                .iter()
                .find(|entry| entry["agent"] == "claude")
                .unwrap_or_else(|| panic!("{action} should export to claude: {payload}"));
            assert!(
                claude_report["error"].is_null(),
                "{action} claude export should succeed: {claude_report}"
            );
            let expected_count = i64::from(expect_deployed);
            let export_counts = claude_report["exports"]
                .as_array()
                .unwrap_or_else(|| panic!("{action} should report claude exports: {payload}"))
                .iter()
                .map(|export| export["exported_count"].as_i64().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                export_counts,
                vec![expected_count, expected_count],
                "{action} should refresh global and local Claude exports"
            );
            let claude_contents = std::fs::read_to_string(&claude_md).unwrap();
            assert_eq!(
                claude_contents.contains("repo-hygiene"),
                expect_deployed,
                "{action} should {} the skill in CLAUDE.md: {claude_contents}",
                if expect_deployed { "deploy" } else { "retract" }
            );
            let local_claude_contents = std::fs::read_to_string(&local_claude_md).unwrap();
            assert_eq!(
                local_claude_contents.contains("repo-hygiene"),
                expect_deployed,
                "{action} should {} the skill in local CLAUDE.md: {local_claude_contents}",
                if expect_deployed { "deploy" } else { "retract" }
            );
        }

        let skill_dir = managed_skill_profile_root
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
        let home = tmp_root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
        let _home_guard = EnvVarGuard::set("HOME", &home);
        let _userprofile_guard = EnvVarGuard::set("USERPROFILE", &home);

        let (cg, host_runtime) = setup_project(&project_root).await;
        let managed_skill_profile_root = host_runtime.profile_root().to_path_buf();
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server_with_host_runtime(
            cg,
            host_runtime,
            dashboard::DashboardTestProjectGraphsV1::default(),
            port,
        );
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
                "source": "user",
                "actor": "dashboard",
                "run_id": null
            }
        });
        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, created) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["state"], "active");
        assert!(
            created["skill"]["metadata"]["created_at"]
                .as_i64()
                .is_some_and(|value| value > 0)
        );
        assert!(
            created["skill"]["metadata"]["updated_at"]
                .as_i64()
                .is_some_and(|value| value > 0)
        );
        assert!(
            managed_skill_profile_root
                .join("agent_managed/skills/repo-hygiene/SKILL.md")
                .is_file(),
            "creating a managed skill must persist a SKILL.md package"
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
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "active"),
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
fn managed_skill_dashboard_api_applies_updates_immediately() {
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
        let home = tmp_root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
        let _home_guard = EnvVarGuard::set("HOME", &home);
        let _userprofile_guard = EnvVarGuard::set("USERPROFILE", &home);

        let (cg, host_runtime) = setup_project(&project_root).await;
        let managed_skill_profile_root = host_runtime.profile_root().to_path_buf();
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server_with_host_runtime(
            cg,
            host_runtime,
            dashboard::DashboardTestProjectGraphsV1::default(),
            port,
        );
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
                "source": "user",
                "actor": "dashboard",
                "run_id": null
            }
        });
        let skills_url = format!("{base_url}/api/automation/skills");
        let skill_url = format!("{skills_url}/repo-hygiene");
        let (status, _) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);

        let active = tracedecay_agent_hosts::automation::managed_skills::load_managed_skill(
            &managed_skill_profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        let base_checksum = active.metadata.checksum.clone();
        let (status, updated) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "base_checksum": base_checksum,
                "summary": "Apply dashboard-visible generated guidance.",
                "body_markdown": "Review the run ledger before applying generated edits.",
                "support_files": [{
                    "path": "templates/review.md",
                    "bytes": [114, 101, 118, 105, 101, 119, 32, 98, 111, 100, 121]
                }]
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(updated["skill"]["metadata"]["state"], "active");
        assert_eq!(
            updated["skill"]["metadata"]["summary"],
            "Apply dashboard-visible generated guidance."
        );
        assert!(updated["skill"]["pending_update"].is_null());
        let skill_dir = managed_skill_profile_root.join("agent_managed/skills/repo-hygiene");
        assert!(!skill_dir.join("references/checklist.md").exists());
        assert!(skill_dir.join("templates/review.md").is_file());

        server.stop();
    });
}

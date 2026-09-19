#[cfg(feature = "test-transport")]
use crate::fixture;
use crate::support::*;
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
#[cfg(feature = "test-transport")]
use tracedecay::mcp::McpServer;
#[cfg(feature = "test-transport")]
use tracedecay::test_support::host_admission::HostAdmissionTestRuntimeV1;
#[cfg(feature = "test-transport")]
use tracedecay_automation_runtime::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState,
    ManagedSupportFile, create_managed_skill, set_managed_skill_state,
};
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifactKind, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    append_run_record, write_run_artifact,
};
#[cfg(feature = "test-transport")]
use tracedecay_automation_runtime::automation::skill_usage::{
    SkillUsageAction, load_skill_usage_record, record_skill_usage,
};

#[tokio::test]
async fn automation_run_artifact_mcp_tool_reads_verified_payload() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let (cg, _env) = init_test_project(&project).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let run_id = "run-mcp-artifact";
    let artifact = write_run_artifact(
        &dashboard_root,
        run_id,
        AutomationRunArtifactKind::CodexHandoff,
        &json!({
            "status": "ready_for_review",
            "next_actions": ["inspect artifact through MCP"],
        }),
        Some("handoff ready".to_string()),
        "1782283200",
    )
    .await
    .unwrap();
    append_run_record(
        &dashboard_root,
        &AutomationRunLedgerRecord {
            schema_version: 2,
            run_id: run_id.to_string(),
            trigger: AutomationTrigger::Dashboard,
            task: tracedecay_automation_runtime::automation::backend::AgentTaskKind::MemoryCurator,
            task_key: Some("memory_curator".to_string()),
            backend: "codex_app_server".to_string(),
            backend_identity: None,
            host_mode: Some("standalone".to_string()),
            prompt_version: Some("memory_curator:v1".to_string()),
            response_schema: None,
            strict_json: Some(true),
            model: Some("test-model".to_string()),
            status: AutomationRunStatus::Succeeded,
            evidence_hash: Some("sha256:evidence".to_string()),
            input_hash: Some("sha256:input".to_string()),
            output_hash: Some("sha256:output".to_string()),
            proposed_ops: Some(json!({"ops": []})),
            applied_ops: None,
            rejected_ops: None,
            validation_report: None,
            reviewed_count: 0,
            accepted_count: 0,
            rejected_count: 0,
            skipped_count: 0,
            error: None,
            error_classification: None,
            error_retryable: None,
            backend_attempt_count: 0,
            backend_attempts: Vec::new(),
            fallback_status: None,
            report_ref: None,
            artifacts: vec![artifact],
            started_at: "1782283199".to_string(),
            completed_at: "1782283200".to_string(),
            completed_at_micros: Some(1_782_283_200_000_000),
        },
    )
    .await
    .unwrap();

    let markdown_result = handle_tool_call(
        &cg,
        "tracedecay_automation_run_artifact_view",
        json!({"run_id": run_id, "kind": "codex_handoff"}),
        None,
        None,
    )
    .await
    .unwrap();
    let markdown_text = extract_text(&markdown_result.value);
    assert!(markdown_text.starts_with("## Automation Run Artifact"));
    assert!(markdown_text.contains("**run_id:** run-mcp-artifact"));
    assert!(markdown_text.contains("**kind:** codex_handoff"));
    assert!(markdown_text.contains("ready_for_review"));
    assert!(!markdown_text.contains("|"));

    let result = handle_tool_call(
        &cg,
        "tracedecay_automation_run_artifact_view",
        json!({"run_id": run_id, "kind": "codex_handoff", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["run_id"], run_id);
    assert_eq!(payload["artifact"]["kind"], "codex_handoff");
    assert_eq!(payload["payload"]["status"], "ready_for_review");
    assert_eq!(
        payload["payload"]["next_actions"][0],
        "inspect artifact through MCP"
    );

    let missing = handle_tool_call(
        &cg,
        "tracedecay_automation_run_artifact_view",
        json!({"run_id": run_id, "kind": "generated_evals"}),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        missing
            .to_string()
            .contains("automation run artifact not found")
    );

    close_test_graph(cg).await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn managed_skill_mcp_tools_list_and_view_profile_store() {
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let home = dir.path().join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let _global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    let cg = TestTraceDecay::new(fixture::init_project_from_template(&project).await.unwrap());
    let profile_root = tracedecay_runtime_core::storage::default_profile_root().unwrap();
    let runtime = open_active_project_scoped_runtime(&cg).await;
    let project_id = HostAdmissionTestRuntimeV1::canonical_project_key(cg.project_root());

    let active_skill = create_managed_skill(
        &profile_root,
        managed_skill_test_draft("active-skill", "Active skill"),
    )
    .await
    .unwrap();
    record_skill_usage(
        &profile_root,
        &active_skill,
        SkillUsageAction::Use,
        "mcp-test",
        vec!["codex".to_string(), "cursor".to_string()],
        Some("codex".to_string()),
        None,
    )
    .await
    .unwrap();
    runtime
        .append_profile_analytics_event_for_test(&tracedecay_global_db::AnalyticsEventInsert {
            provider: "mcp".to_string(),
            project_id: project_id.clone(),
            session_id: Some("mcp-skill-session".to_string()),
            timestamp: tracedecay::project::current_timestamp(),
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
                        "arguments": { "id": "active-skill" }
                    }
                })
                .to_string(),
            ),
        })
        .await
        .unwrap();
    let server =
        McpServer::new_with_host_admission_test_runtime_for_test(cg.into_inner(), None, runtime)
            .await
            .expect("registered test server");

    let markdown_list = server
        .call_tool_for_test("tracedecay_skill_list", json!({"state": "active"}))
        .await
        .unwrap();
    let markdown_text = extract_text(&markdown_list.value);
    assert!(markdown_text.starts_with("## Managed Skills"));
    assert!(markdown_text.contains("**count:** 1"));
    assert!(markdown_text.contains("**active-skill**"));
    assert!(markdown_text.contains("Active skill"));
    assert!(!markdown_text.contains("|"));

    let list = server
        .call_tool_for_test(
            "tracedecay_skill_list",
            json!({"state": "active", "format": "json"}),
        )
        .await
        .unwrap();
    assert!(list.touched_files.is_empty());
    let payload = extract_json(&list.value);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["skills"][0]["metadata"]["id"], "active-skill");
    assert_eq!(payload["skills"][0]["metadata"]["state"], "active");
    assert_eq!(payload["skills"][0]["support_file_count"], 1);
    assert_eq!(payload["skills"][0]["usage_summary"]["view_count"], 1);
    assert_eq!(payload["skills"][0]["usage_summary"]["use_count"], 1);
    assert_eq!(
        payload["skills"][0]["usage_summary"]["targets"],
        json!(["codex", "cursor", "mcp"])
    );
    assert_eq!(
        payload["skills"][0]["stale_recommendation"]["skill_id"],
        "active-skill"
    );
    assert_eq!(
        payload["skills"][0]["improvement_recommendation"]["skill_id"],
        "active-skill"
    );
    assert_eq!(
        payload["skills"][0]["improvement_recommendation"]["recommendation"],
        "none"
    );
    assert!(payload["skills"][0].get("body_markdown").is_none());

    let markdown_view = server
        .call_tool_for_test(
            "tracedecay_skill_view",
            json!({
                "id": "active-skill",
                "include_support_files": false,
                "__mcp_request_id": "req-active-view",
            }),
        )
        .await
        .unwrap();
    let markdown_text = extract_text(&markdown_view.value);
    assert!(markdown_text.starts_with("## Managed Skill: active-skill"));
    assert!(markdown_text.contains("**state:** active"));
    assert!(markdown_text.contains("### Body"));
    assert!(markdown_text.contains("Active skill"));
    assert!(!markdown_text.contains("|"));

    let view = server
        .call_tool_for_test(
            "tracedecay_skill_view",
            json!({
                "id": "active-skill",
                "__mcp_request_id": "req-active-view",
                "format": "json",
            }),
        )
        .await
        .unwrap();
    assert!(view.touched_files.is_empty());
    let payload = extract_json(&view.value);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["skill"]["metadata"]["id"], "active-skill");
    assert_eq!(payload["usage_summary"]["view_count"], 3);
    assert_eq!(payload["usage_summary"]["use_count"], 1);
    assert!(
        payload["usage_summary"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|target| target == "mcp"),
        "direct MCP view should mark mcp as a usage target: {payload:#}"
    );
    assert_eq!(payload["stale_recommendation"]["skill_id"], "active-skill");
    assert_eq!(
        payload["improvement_recommendation"]["skill_id"],
        "active-skill"
    );
    assert!(
        payload["skill"]["body_markdown"]
            .as_str()
            .unwrap()
            .contains("Active skill")
    );
    assert_eq!(
        payload["skill"]["support_files"].as_array().unwrap().len(),
        0
    );
    assert_eq!(payload["support_files_included"], false);
    let summary = &payload["support_file_summaries"][0];
    assert_eq!(summary["path"], "references/checklist.md");
    assert!(summary["byte_len"].as_u64().unwrap() > 0);
    let encoded = payload.to_string();
    assert!(
        !encoded.contains("inspect context"),
        "default skill view must not inline support-file bytes: {encoded}"
    );
    let usage_record = load_skill_usage_record(&profile_root, "active-skill")
        .await
        .unwrap()
        .expect("skill view should write direct usage telemetry");
    assert_eq!(usage_record.view_count, 3);
    assert_eq!(usage_record.use_count, 1);
    assert!(usage_record.targets.iter().any(|target| target == "mcp"));

    server
        .host_admission_test_runtime_for_test()
        .expect("server should retain the host-admission test runtime")
        .append_profile_analytics_event_for_test(&tracedecay_global_db::AnalyticsEventInsert {
            provider: "mcp".to_string(),
            project_id,
            session_id: Some("mcp-skill-session".to_string()),
            timestamp: tracedecay::project::current_timestamp(),
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
                    "request_id": "req-active-view",
                    "function": {
                        "name": "tracedecay_skill_view",
                        "arguments": { "id": "active-skill" }
                    }
                })
                .to_string(),
            ),
        })
        .await
        .unwrap();
    let list_after_view = server
        .call_tool_for_test(
            "tracedecay_skill_list",
            json!({"state": "active", "format": "json"}),
        )
        .await
        .unwrap();
    let payload = extract_json(&list_after_view.value);
    assert_eq!(payload["skills"][0]["usage_summary"]["view_count"], 3);

    drop(server);
    drop(env_lock);
}

#[cfg(feature = "test-transport")]
pub(crate) fn managed_skill_test_draft(id: &str, title: &str) -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: id.to_string(),
        title: title.to_string(),
        summary: format!("{title} summary."),
        routing_description: format!("{title} summary."),
        category: "maintenance".to_string(),
        targets:
            tracedecay_automation_runtime::automation::managed_skills::default_managed_skill_targets(
            ),
        body_markdown: format!("Use {title} before applying repository changes."),
        support_files: vec![
            ManagedSupportFile::new(
                "references/checklist.md",
                b"- inspect context\n- run focused tests\n".to_vec(),
            )
            .unwrap(),
        ],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "tracedecay-test".to_string(),
            run_id: Some("run_mcp_skill".to_string()),
        },
    }
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn skill_list_returns_filtered_skills_and_rejects_unknown_state() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let (cg, _env) = init_test_project(&project).await;
    let profile_root = tracedecay_runtime_core::storage::default_profile_root().unwrap();
    let profile = profile_root.display().to_string();
    let runtime = open_active_project_scoped_runtime(&cg).await;
    let server =
        McpServer::new_with_host_admission_test_runtime_for_test(cg.into_inner(), None, runtime)
            .await
            .expect("registered test server");

    let empty = server
        .call_tool_for_test("tracedecay_skill_list", json!({"format": "json"}))
        .await
        .unwrap();
    assert_eq!(empty.touched_files, Vec::<String>::new());
    assert_eq!(
        extract_json(&empty.value),
        json!({
            "status": "ok",
            "profile_root": profile,
            "count": 0,
            "skills": []
        })
    );
    let empty_markdown = server
        .call_tool_for_test("tracedecay_skill_list", json!({}))
        .await
        .unwrap();
    assert_eq!(
        extract_text(&empty_markdown.value),
        format!(
            "\
## Managed Skills
**status:** ok
**count:** 0
**profile_root:** {profile}

### Skills
_No managed skills._
"
        )
    );

    create_listed_skill(
        &profile_root,
        "alpha-active",
        "Alpha Active",
        "Lists the active skill.",
        "Alpha body is the active skill text.",
    )
    .await;
    create_listed_skill(
        &profile_root,
        "beta-disabled",
        "Beta Disabled",
        "Lists the disabled skill.",
        "Beta body is the disabled skill text.",
    )
    .await;
    create_listed_skill(
        &profile_root,
        "gamma-archived",
        "Gamma Archived",
        "Lists the archived skill.",
        "Gamma body is the archived skill text.",
    )
    .await;
    set_managed_skill_state(&profile_root, "beta-disabled", ManagedSkillState::Disabled)
        .await
        .unwrap();
    set_managed_skill_state(&profile_root, "gamma-archived", ManagedSkillState::Archived)
        .await
        .unwrap();
    let records_before = skill_records(&profile_root);

    let unknown = server
        .call_tool_for_test(
            "tracedecay_skill_list",
            json!({"state": "retired", "format": "json"}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        unknown.to_string(),
        "config error: unknown managed skill state: retired"
    );
    assert_eq!(skill_records(&profile_root), records_before);

    let active = server
        .call_tool_for_test(
            "tracedecay_skill_list",
            json!({"state": "active", "include_body": true, "format": "json"}),
        )
        .await
        .unwrap();
    assert_eq!(active.touched_files, Vec::<String>::new());
    let active_payload = extract_json(&active.value);
    assert_eq!(active_payload["status"], "ok");
    assert_eq!(active_payload["count"], 1);
    assert_eq!(active_payload["profile_root"], profile);
    assert_eq!(
        listed_skills_without_clocks(&active_payload),
        vec![listed_skill(
            "alpha-active",
            "Alpha Active",
            "Lists the active skill.",
            "active",
            Some("Alpha body is the active skill text."),
            0,
            &[],
            json!({
                "skill_id": "alpha-active",
                "stale": true,
                "recommendation": "archive_candidate",
                "reason": "no view, use, or patch activity has been recorded"
            }),
            json!({
                "skill_id": "alpha-active",
                "improvement": false,
                "recommendation": "none",
                "reason": "no repeated correction or failed-use signal is present",
                "priority": "none"
            }),
        )]
    );
    assert_eq!(
        recommendation_evidence(&active_payload["skills"][0]["stale_recommendation"]),
        vec![
            "state=active".to_string(),
            "pinned=false".to_string(),
            "views=0".to_string(),
            "uses=0".to_string(),
            "patches=0".to_string(),
            "last_activity_at=0".to_string(),
            "created_by=skill-list-proof".to_string(),
            "provenance_source=automation_run".to_string(),
        ]
    );

    let disabled = server
        .call_tool_for_test(
            "tracedecay_skill_list",
            json!({"state": "disabled", "format": "json"}),
        )
        .await
        .unwrap();
    let disabled_payload = extract_json(&disabled.value);
    assert_eq!(disabled_payload["count"], 1);
    assert_eq!(
        listed_skills_without_clocks(&disabled_payload),
        vec![listed_skill(
            "beta-disabled",
            "Beta Disabled",
            "Lists the disabled skill.",
            "disabled",
            None,
            1,
            &["lifecycle"],
            json!({
                "skill_id": "beta-disabled",
                "stale": false,
                "recommendation": "keep",
                "reason": "disabled skills are not auto-archive candidates"
            }),
            json!({
                "skill_id": "beta-disabled",
                "improvement": false,
                "recommendation": "none",
                "reason": "disabled or archived skills are not patch recommendation candidates",
                "priority": "none"
            }),
        )]
    );
    assert_eq!(
        recommendation_evidence(&disabled_payload["skills"][0]["stale_recommendation"]),
        vec![
            "state=disabled".to_string(),
            "pinned=false".to_string(),
            "views=0".to_string(),
            "uses=0".to_string(),
            "patches=1".to_string(),
            "created_by=skill-list-proof".to_string(),
            "provenance_source=automation_run".to_string(),
        ]
    );

    let archived = server
        .call_tool_for_test(
            "tracedecay_skill_list",
            json!({"state": "archived", "include_body": true, "format": "json"}),
        )
        .await
        .unwrap();
    let archived_payload = extract_json(&archived.value);
    assert_eq!(archived_payload["count"], 1);
    assert_eq!(
        listed_skills_without_clocks(&archived_payload),
        vec![listed_skill(
            "gamma-archived",
            "Gamma Archived",
            "Lists the archived skill.",
            "archived",
            Some("Gamma body is the archived skill text."),
            1,
            &["lifecycle"],
            json!({
                "skill_id": "gamma-archived",
                "stale": false,
                "recommendation": "keep",
                "reason": "skill is already archived"
            }),
            json!({
                "skill_id": "gamma-archived",
                "improvement": false,
                "recommendation": "none",
                "reason": "disabled or archived skills are not patch recommendation candidates",
                "priority": "none"
            }),
        )]
    );

    let listed = server
        .call_tool_for_test("tracedecay_skill_list", json!({"format": "json"}))
        .await
        .unwrap();
    let listed_payload = extract_json(&listed.value);
    assert_eq!(listed_payload["status"], "ok");
    assert_eq!(listed_payload["count"], 3);
    assert_eq!(listed_payload["profile_root"], profile);
    assert_eq!(
        listed_payload["skills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|skill| skill["metadata"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["alpha-active", "beta-disabled", "gamma-archived"]
    );
    assert_eq!(
        listed_payload["skills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|skill| skill["metadata"]["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["active", "disabled", "archived"]
    );

    let markdown = server
        .call_tool_for_test("tracedecay_skill_list", json!({}))
        .await
        .unwrap();
    assert_eq!(markdown.touched_files, Vec::<String>::new());
    assert_eq!(
        extract_text(&markdown.value),
        format!(
            "\
## Managed Skills
**status:** ok
**count:** 3
**profile_root:** {profile}

### Skills
- **alpha-active** - Alpha Active (active)
  summary: Lists the active skill.
  category: maintenance; targets: cursor, codex, claude, agents, opencode, kimi, kiro, hermes; support_files: 1
- **beta-disabled** - Beta Disabled (disabled)
  summary: Lists the disabled skill.
  category: maintenance; targets: cursor, codex, claude, agents, opencode, kimi, kiro, hermes; support_files: 1
- **gamma-archived** - Gamma Archived (archived)
  summary: Lists the archived skill.
  category: maintenance; targets: cursor, codex, claude, agents, opencode, kimi, kiro, hermes; support_files: 1
"
        )
    );
    let active_markdown = server
        .call_tool_for_test("tracedecay_skill_list", json!({"state": "active"}))
        .await
        .unwrap();
    assert_eq!(
        extract_text(&active_markdown.value),
        format!(
            "\
## Managed Skills
**status:** ok
**count:** 1
**profile_root:** {profile}

### Skills
- **alpha-active** - Alpha Active (active)
  summary: Lists the active skill.
  category: maintenance; targets: cursor, codex, claude, agents, opencode, kimi, kiro, hermes; support_files: 1
"
        )
    );
    assert_eq!(skill_records(&profile_root), records_before);

    drop(server);
}

#[cfg(feature = "test-transport")]
async fn create_listed_skill(
    profile_root: &std::path::Path,
    id: &str,
    title: &str,
    summary: &str,
    body: &str,
) {
    create_managed_skill(
        profile_root,
        ManagedSkillDraft {
            id: id.to_string(),
            title: title.to_string(),
            summary: summary.to_string(),
            routing_description: summary.to_string(),
            category: "maintenance".to_string(),
            targets: tracedecay_automation_runtime::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: body.to_string(),
            support_files: vec![
                ManagedSupportFile::new(
                    "references/checklist.md",
                    b"inspect the listed skill\n".to_vec(),
                )
                .unwrap(),
            ],
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::AutomationRun,
                actor: "skill-list-proof".to_string(),
                run_id: Some("run-skill-list-proof".to_string()),
            },
        },
    )
    .await
    .unwrap();
}

#[cfg(feature = "test-transport")]
fn skill_records(profile_root: &std::path::Path) -> Vec<Vec<u8>> {
    ["alpha-active", "beta-disabled", "gamma-archived"]
        .into_iter()
        .map(|id| {
            fs::read(
                profile_root
                    .join("agent_managed")
                    .join("skills")
                    .join(id)
                    .join("skill.json"),
            )
            .unwrap_or_else(|error| panic!("read {id} skill record: {error}"))
        })
        .collect()
}

#[cfg(feature = "test-transport")]
fn listed_skills_without_clocks(payload: &Value) -> Vec<Value> {
    payload["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .map(skill_list_item_without_clocks)
        .collect()
}

#[cfg(feature = "test-transport")]
fn skill_list_item_without_clocks(skill: &Value) -> Value {
    let mut skill = skill.clone();
    if let Some(metadata) = skill.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.remove("checksum");
        metadata.remove("created_at");
        metadata.remove("updated_at");
        metadata.remove("activated_at");
    }
    if let Some(usage) = skill
        .get_mut("usage_summary")
        .and_then(Value::as_object_mut)
    {
        usage.remove("activated_at");
        usage.remove("last_activity_at");
        usage.remove("last_patched_at");
    }
    for key in ["stale_recommendation", "improvement_recommendation"] {
        if let Some(recommendation) = skill.get_mut(key).and_then(Value::as_object_mut) {
            recommendation.remove("evidence");
        }
    }
    skill
}

#[cfg(feature = "test-transport")]
fn recommendation_evidence(recommendation: &Value) -> Vec<String> {
    recommendation["evidence"]
        .as_array()
        .expect("recommendation evidence")
        .iter()
        .filter_map(Value::as_str)
        .filter(|line| !line.starts_with("activated_at="))
        .filter(|line| match line.strip_prefix("last_activity_at=") {
            Some(activity) => activity == "0",
            None => true,
        })
        .map(str::to_string)
        .collect()
}

#[cfg(feature = "test-transport")]
fn listed_skill(
    id: &str,
    title: &str,
    summary: &str,
    state: &str,
    body: Option<&str>,
    patch_count: u64,
    usage_targets: &[&str],
    stale_recommendation: Value,
    improvement_recommendation: Value,
) -> Value {
    let mut skill = json!({
        "metadata": {
            "id": id,
            "title": title,
            "summary": summary,
            "routing_description": summary,
            "category": "maintenance",
            "targets": ["cursor", "codex", "claude", "agents", "opencode", "kimi", "kiro", "hermes"],
            "state": state,
            "pinned": false,
            "provenance": {
                "source": "automation_run",
                "actor": "skill-list-proof",
                "run_id": "run-skill-list-proof"
            }
        },
        "support_file_count": 1,
        "support_file_paths": ["references/checklist.md"],
        "usage_summary": {
            "schema_version": 2,
            "skill_id": id,
            "title": title,
            "category": "maintenance",
            "state": state,
            "pinned": false,
            "created_by": "skill-list-proof",
            "provenance_source": "automation_run",
            "targets": usage_targets,
            "view_count": 0,
            "use_count": 0,
            "patch_count": patch_count,
            "first_seen_at": 0,
            "last_viewed_at": null,
            "last_used_at": null,
            "view_count_at_activation": 0,
            "use_count_at_activation": 0
        },
        "stale_recommendation": stale_recommendation,
        "improvement_recommendation": improvement_recommendation
    });
    if let Some(body) = body {
        skill["body_markdown"] = json!(body);
    }
    skill
}

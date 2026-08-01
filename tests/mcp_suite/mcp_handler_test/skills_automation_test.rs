#[cfg(feature = "test-transport")]
use crate::fixture;
use crate::support::*;
use serde_json::json;
use std::fs;
use tempfile::TempDir;
#[cfg(feature = "test-transport")]
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
#[cfg(feature = "test-transport")]
use tracedecay::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSupportFile,
    approve_managed_skill, create_managed_skill_draft,
};
use tracedecay::automation::run_ledger::{
    AutomationRunArtifactKind, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    append_run_record, write_run_artifact,
};
#[cfg(feature = "test-transport")]
use tracedecay::automation::skill_usage::{
    SkillUsageAction, load_skill_usage_record, record_skill_usage,
};
#[cfg(feature = "test-transport")]
use tracedecay::mcp::McpServer;

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
            task: tracedecay::automation::backend::AgentTaskKind::MemoryCurator,
            task_key: Some("memory_curator".to_string()),
            backend: "codex_app_server".to_string(),
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
    let profile_root = tracedecay::storage::default_profile_root().unwrap();
    let runtime = open_active_project_scoped_runtime(&cg).await;
    let project_id = HostAdmissionTestRuntimeV1::canonical_project_key(cg.project_root());

    create_managed_skill_draft(
        &profile_root,
        managed_skill_test_draft("pending-skill", "Pending skill"),
    )
    .await
    .unwrap();
    create_managed_skill_draft(
        &profile_root,
        managed_skill_test_draft("active-skill", "Active skill"),
    )
    .await
    .unwrap();
    let active_skill = approve_managed_skill(&profile_root, "active-skill")
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
        .append_profile_analytics_event_for_test(&tracedecay::global_db::AnalyticsEventInsert {
            provider: "mcp".to_string(),
            project_id: project_id.clone(),
            session_id: Some("mcp-skill-session".to_string()),
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
        json!(["codex", "cursor", "lifecycle", "mcp"])
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
                "include_support_files": false,
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
        .append_profile_analytics_event_for_test(&tracedecay::global_db::AnalyticsEventInsert {
            provider: "mcp".to_string(),
            project_id,
            session_id: Some("mcp-skill-session".to_string()),
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
        category: "maintenance".to_string(),
        targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
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

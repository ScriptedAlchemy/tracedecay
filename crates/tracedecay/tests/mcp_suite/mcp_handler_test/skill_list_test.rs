//! Caller-visible behavior of `tracedecay_skill_list`.
//!
//! The tool is the read-only inventory of the active profile's managed
//! skills. These assertions name the skill the caller stored and the
//! lifecycle they asked for, so a filter that ignores `state`, a body that
//! appears without `include_body`, or a repeat call that invents usage fails.

use std::fs;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::mcp::McpServer;
use tracedecay_automation_runtime::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState,
    ManagedSupportFile, SkillInstallTarget, create_managed_skill, set_managed_skill_state,
};

use crate::fixture;
use crate::support::{
    GLOBAL_DB_ENV_LOCK, GlobalDbEnvGuard, HomeEnvGuard, TestTraceDecay, extract_json, extract_text,
    open_active_project_scoped_runtime,
};

const ACTOR: &str = "skill-list-proof";

#[tokio::test]
async fn skill_list_returns_stored_skills_for_the_requested_state() {
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn skill_list_marker() {}\n",
    )
    .unwrap();
    let home = dir.path().join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let _global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    let cg = TestTraceDecay::new(fixture::init_project_from_template(&project).await.unwrap());
    let profile_root = tracedecay_runtime_core::storage::default_profile_root().unwrap();
    let profile_root_text = profile_root.display().to_string();
    let runtime = open_active_project_scoped_runtime(&cg).await;

    create_managed_skill(&profile_root, active_draft())
        .await
        .unwrap();
    let disabled = create_managed_skill(&profile_root, disabled_draft())
        .await
        .unwrap();
    set_managed_skill_state(
        &profile_root,
        &disabled.metadata.id,
        ManagedSkillState::Disabled,
    )
    .await
    .unwrap();
    let archived = create_managed_skill(&profile_root, archived_draft())
        .await
        .unwrap();
    set_managed_skill_state(
        &profile_root,
        &archived.metadata.id,
        ManagedSkillState::Archived,
    )
    .await
    .unwrap();

    let server =
        McpServer::new_with_host_admission_test_runtime_for_test(cg.into_inner(), None, runtime)
            .await
            .expect("registered test server");

    let all = call_skill_list(&server, json!({"format": "json"})).await;
    assert_eq!(all["status"], "ok");
    assert_eq!(all["profile_root"], profile_root_text);
    assert_eq!(all["count"], 3);
    assert_eq!(
        listed(&all),
        vec![active_listing(), archived_listing(), disabled_listing()]
    );
    assert_eq!(all["skills"][0].get("body_markdown"), None);
    assert_eq!(all["skills"][0]["usage_summary"]["first_seen_at"], 0);
    assert_eq!(all["skills"][0]["usage_summary"]["last_activity_at"], 0);
    assert!(
        !all.to_string().contains("checklist-line"),
        "skill list must not inline support-file bytes: {all}"
    );

    let active = call_skill_list(&server, json!({"state": "active", "format": "json"})).await;
    assert_eq!(active["status"], "ok");
    assert_eq!(active["count"], 1);
    assert_eq!(listed(&active), vec![active_listing()]);

    let with_body = call_skill_list(
        &server,
        json!({"state": "active", "include_body": true, "format": "json"}),
    )
    .await;
    assert_eq!(with_body["count"], 1);
    assert_eq!(
        with_body["skills"][0]["body_markdown"],
        "Active skill body."
    );
    assert_eq!(with_body["skills"][0]["metadata"]["id"], "skill-active");

    let disabled_only =
        call_skill_list(&server, json!({"state": "disabled", "format": "json"})).await;
    assert_eq!(disabled_only["count"], 1);
    assert_eq!(listed(&disabled_only), vec![disabled_listing()]);

    let archived_only =
        call_skill_list(&server, json!({"state": "archived", "format": "json"})).await;
    assert_eq!(archived_only["count"], 1);
    assert_eq!(listed(&archived_only), vec![archived_listing()]);

    let again = call_skill_list(&server, json!({"state": "active", "format": "json"})).await;
    assert_eq!(listed(&again), vec![active_listing()]);
    assert_eq!(again["skills"][0]["usage_summary"]["view_count"], 0);

    let markdown = call_skill_list_text(&server, json!({"state": "active"})).await;
    assert_eq!(
        markdown,
        format!(
            "## Managed Skills\n\
             **status:** ok\n\
             **count:** 1\n\
             **profile_root:** {profile_root_text}\n\
             \n\
             ### Skills\n\
             - **skill-active** - Active skill (active)\n\
               summary: Active skill summary.\n\
               category: maintenance; targets: cursor, codex; support_files: 1\n"
        )
    );

    let rejected = server
        .call_tool_for_test(
            "tracedecay_skill_list",
            json!({"state": "retired", "format": "json"}),
        )
        .await
        .expect_err("unknown lifecycle state must be rejected");
    assert_eq!(
        rejected.to_string(),
        "config error: unknown managed skill state: retired"
    );

    drop(server);
    drop(env_lock);
}

async fn call_skill_list(server: &McpServer, args: Value) -> Value {
    let result = server
        .call_tool_for_test("tracedecay_skill_list", args)
        .await
        .expect("tracedecay_skill_list");
    assert!(
        result.touched_files.is_empty(),
        "skill list must not report file edits: {:?}",
        result.touched_files
    );
    extract_json(&result.value)
}

async fn call_skill_list_text(server: &McpServer, args: Value) -> String {
    let result = server
        .call_tool_for_test("tracedecay_skill_list", args)
        .await
        .expect("tracedecay_skill_list markdown");
    extract_text(&result.value).to_string()
}

fn listed(payload: &Value) -> Vec<Value> {
    payload["skills"]
        .as_array()
        .expect("skills")
        .iter()
        .map(listed_skill)
        .collect()
}

fn listed_skill(skill: &Value) -> Value {
    let metadata = &skill["metadata"];
    let usage = &skill["usage_summary"];
    let stale = &skill["stale_recommendation"];
    let improvement = &skill["improvement_recommendation"];
    json!({
        "id": metadata["id"],
        "title": metadata["title"],
        "summary": metadata["summary"],
        "routing_description": metadata["routing_description"],
        "category": metadata["category"],
        "targets": metadata["targets"],
        "state": metadata["state"],
        "pinned": metadata["pinned"],
        "source": metadata["provenance"]["source"],
        "actor": metadata["provenance"]["actor"],
        "run_id": metadata["provenance"]["run_id"],
        "support_file_count": skill["support_file_count"],
        "support_file_paths": skill["support_file_paths"],
        "views": usage["view_count"],
        "uses": usage["use_count"],
        "patches": usage["patch_count"],
        "usage_targets": usage["targets"],
        "usage_state": usage["state"],
        "created_by": usage["created_by"],
        "provenance_source": usage["provenance_source"],
        "views_at_activation": usage["view_count_at_activation"],
        "uses_at_activation": usage["use_count_at_activation"],
        "stale": stale["stale"],
        "stale_skill_id": stale["skill_id"],
        "stale_recommendation": stale["recommendation"],
        "stale_reason": stale["reason"],
        "improvement": improvement["improvement"],
        "improvement_skill_id": improvement["skill_id"],
        "improvement_recommendation": improvement["recommendation"],
        "improvement_reason": improvement["reason"],
        "improvement_priority": improvement["priority"],
    })
}

fn active_listing() -> Value {
    json!({
        "id": "skill-active",
        "title": "Active skill",
        "summary": "Active skill summary.",
        "routing_description": "Active skill summary.",
        "category": "maintenance",
        "targets": ["cursor", "codex"],
        "state": "active",
        "pinned": false,
        "source": "automation_run",
        "actor": ACTOR,
        "run_id": "run-active",
        "support_file_count": 1,
        "support_file_paths": ["references/checklist.md"],
        "views": 0,
        "uses": 0,
        "patches": 0,
        "usage_targets": [],
        "usage_state": "active",
        "created_by": ACTOR,
        "provenance_source": "automation_run",
        "views_at_activation": 0,
        "uses_at_activation": 0,
        "stale": true,
        "stale_skill_id": "skill-active",
        "stale_recommendation": "archive_candidate",
        "stale_reason": "no view, use, or patch activity has been recorded",
        "improvement": false,
        "improvement_skill_id": "skill-active",
        "improvement_recommendation": "none",
        "improvement_reason": "no repeated correction or failed-use signal is present",
        "improvement_priority": "none",
    })
}

fn disabled_listing() -> Value {
    json!({
        "id": "skill-disabled",
        "title": "Disabled skill",
        "summary": "Disabled skill summary.",
        "routing_description": "Disabled skill summary.",
        "category": "review",
        "targets": ["claude"],
        "state": "disabled",
        "pinned": false,
        "source": "automation_run",
        "actor": ACTOR,
        "run_id": "run-disabled",
        "support_file_count": 0,
        "support_file_paths": [],
        "views": 0,
        "uses": 0,
        "patches": 1,
        "usage_targets": ["lifecycle"],
        "usage_state": "disabled",
        "created_by": ACTOR,
        "provenance_source": "automation_run",
        "views_at_activation": 0,
        "uses_at_activation": 0,
        "stale": false,
        "stale_skill_id": "skill-disabled",
        "stale_recommendation": "keep",
        "stale_reason": "disabled skills are not auto-archive candidates",
        "improvement": false,
        "improvement_skill_id": "skill-disabled",
        "improvement_recommendation": "none",
        "improvement_reason": "disabled or archived skills are not patch recommendation candidates",
        "improvement_priority": "none",
    })
}

fn archived_listing() -> Value {
    json!({
        "id": "skill-archived",
        "title": "Archived skill",
        "summary": "Archived skill summary.",
        "routing_description": "Archived skill summary.",
        "category": "history",
        "targets": ["hermes"],
        "state": "archived",
        "pinned": false,
        "source": "automation_run",
        "actor": ACTOR,
        "run_id": "run-archived",
        "support_file_count": 0,
        "support_file_paths": [],
        "views": 0,
        "uses": 0,
        "patches": 1,
        "usage_targets": ["lifecycle"],
        "usage_state": "archived",
        "created_by": ACTOR,
        "provenance_source": "automation_run",
        "views_at_activation": 0,
        "uses_at_activation": 0,
        "stale": false,
        "stale_skill_id": "skill-archived",
        "stale_recommendation": "keep",
        "stale_reason": "skill is already archived",
        "improvement": false,
        "improvement_skill_id": "skill-archived",
        "improvement_recommendation": "none",
        "improvement_reason": "disabled or archived skills are not patch recommendation candidates",
        "improvement_priority": "none",
    })
}

fn active_draft() -> ManagedSkillDraft {
    draft(
        "skill-active",
        "Active skill",
        "maintenance",
        "Active skill body.",
        vec![SkillInstallTarget::Cursor, SkillInstallTarget::Codex],
        "run-active",
        vec![
            ManagedSupportFile::new("references/checklist.md", b"checklist-line\n".to_vec())
                .unwrap(),
        ],
    )
}

fn disabled_draft() -> ManagedSkillDraft {
    draft(
        "skill-disabled",
        "Disabled skill",
        "review",
        "Disabled skill body.",
        vec![SkillInstallTarget::Claude],
        "run-disabled",
        Vec::new(),
    )
}

fn archived_draft() -> ManagedSkillDraft {
    draft(
        "skill-archived",
        "Archived skill",
        "history",
        "Archived skill body.",
        vec![SkillInstallTarget::Hermes],
        "run-archived",
        Vec::new(),
    )
}

fn draft(
    id: &str,
    title: &str,
    category: &str,
    body: &str,
    targets: Vec<SkillInstallTarget>,
    run_id: &str,
    support_files: Vec<ManagedSupportFile>,
) -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: id.to_string(),
        title: title.to_string(),
        summary: format!("{title} summary."),
        routing_description: format!("{title} summary."),
        category: category.to_string(),
        targets,
        body_markdown: body.to_string(),
        support_files,
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: ACTOR.to_string(),
            run_id: Some(run_id.to_string()),
        },
    }
}

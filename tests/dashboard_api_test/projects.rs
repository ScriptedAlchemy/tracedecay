use crate::dashboard_api_support::*;
use std::path::PathBuf;

async fn setup_target_project(fixture: &DashboardFixture) -> (PathBuf, TraceDecay) {
    let target_root = fixture
        ._tmp
        .path()
        .canonicalize()
        .expect("fixture root should canonicalize")
        .join("target-project");
    let target_cg = setup_project(&target_root).await;
    (target_root, target_cg)
}

fn project_id(cg: &TraceDecay) -> String {
    cg.store_layout()
        .identity
        .project_id
        .clone()
        .expect("profile-backed target should have project_id")
}

#[test]
fn dashboard_projects_endpoint_lists_registered_projects_and_active_project() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent();

        let (target_root, target_cg) = setup_target_project(&fixture).await;
        drop(target_cg);

        let (status, projects) = get_json(&agent, &format!("{}/api/projects", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(projects["status"], "ok");
        assert_eq!(
            projects["active_project_root"],
            fixture.project_root.display().to_string()
        );
        let rows = projects["projects"]
            .as_array()
            .unwrap_or_else(|| panic!("expected project list array: {projects}"));
        assert!(
            rows.iter().any(|row| row["project_root"]
                == fixture.project_root.display().to_string()
                && row["is_active"] == true),
            "active project should be identified in daemon project list: {projects}"
        );
        assert!(
            rows.iter().any(
                |row| row["project_root"] == target_root.display().to_string()
                    && row["is_active"] == false
            ),
            "other registered project should be listed for selection: {projects}"
        );
    });
}

#[test]
fn project_scoped_plugin_routes_read_selected_project_store() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent_with_timeout(std::time::Duration::from_secs(20));

        let (_target_root, target_cg) = setup_target_project(&fixture).await;
        let target_project_id = project_id(&target_cg);
        target_cg
            .db()
            .conn()
            .execute(
                "INSERT INTO memory_facts
                    (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                libsql::params![
                    201_i64,
                    "Target daemon project selector fact",
                    "project",
                    "[\"selector\"]",
                    0.91_f64,
                    1_i64,
                    1_i64,
                    1_700_010_000_i64,
                    1_700_010_100_i64
                ],
            )
            .await
            .expect("target fact should insert");
        target_cg
            .checkpoint()
            .await
            .expect("target project DB should checkpoint before dashboard reopen");
        target_cg.close();

        let (active_status, active_payload) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/?q=selector&limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(active_status, 200);
        assert_eq!(
            active_payload["holographic"]["facts"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "active project should not contain target-only selector fact"
        );

        let (selected_status, selected_payload) = get_json(
            &agent,
            &format!(
                "{}/api/projects/{}/plugins/holographic/?q=selector&limit=10",
                fixture.base_url, target_project_id
            ),
        );
        assert_eq!(selected_status, 200);
        let selected_facts = selected_payload["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected selected project facts: {selected_payload}"));
        assert_eq!(selected_facts.len(), 1);
        assert_eq!(
            selected_facts[0]["content"],
            "Target daemon project selector fact"
        );
    });
}

#[test]
fn project_scoped_curation_preview_and_activity_do_not_leak_active_state() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent_with_timeout(std::time::Duration::from_secs(20));

        let (_target_root, target_cg) = setup_target_project(&fixture).await;
        let target_project_id = project_id(&target_cg);
        target_cg
            .checkpoint()
            .await
            .expect("target project DB should checkpoint before dashboard reopen");
        target_cg.close();

        let (active_curate_status, active_curate) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(active_curate_status, 200);
        assert_eq!(active_curate["dry_run"], true);

        let (active_preview_status, active_preview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(active_preview_status, 200);
        assert!(
            !active_preview["report"].is_null(),
            "active dry-run should save a preview"
        );
        let (active_activity_status, active_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=75",
                fixture.base_url
            ),
        );
        assert_eq!(active_activity_status, 200);
        assert!(
            active_activity["count"].as_i64().unwrap_or_default() > 0,
            "active dry-run should record curation activity"
        );

        let (selected_preview_status, selected_preview) = get_json(
            &agent,
            &format!(
                "{}/api/projects/{}/plugins/holographic/curation/preview",
                fixture.base_url, target_project_id
            ),
        );
        assert_eq!(selected_preview_status, 200);
        assert!(
            selected_preview["report"].is_null(),
            "selected non-active project must not reuse the active preview: {selected_preview}"
        );

        let (selected_activity_status, selected_activity) = get_json(
            &agent,
            &format!(
                "{}/api/projects/{}/plugins/holographic/curation/activity?limit=75",
                fixture.base_url, target_project_id
            ),
        );
        assert_eq!(selected_activity_status, 200);
        assert_eq!(
            selected_activity["count"], 0,
            "selected non-active project must not reuse active curation activity: {selected_activity}"
        );
    });
}

#[test]
fn project_scoped_mutations_are_rejected_for_non_active_projects() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent_with_timeout(std::time::Duration::from_secs(20));

        let (_target_root, target_cg) = setup_target_project(&fixture).await;
        let target_project_id = project_id(&target_cg);
        target_cg.close();

        let (status, body) = post_json_body(
            &agent,
            &format!(
                "{}/api/projects/{}/plugins/holographic/curate",
                fixture.base_url, target_project_id
            ),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 405);
        assert_eq!(body["status"], "read_only_project");
    });
}

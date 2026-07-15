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
        let target_project_id = project_id(&target_cg);
        drop(target_cg);

        // Register a credential-bearing git remote for the target project
        // via the same seeding path production code uses, so the
        // redaction assertion below is actually exercised instead of
        // passing vacuously on an absent field.
        let credential_remote_url = "https://user:sekret-token@github.com/example/target.git";
        {
            let global_db = GlobalDb::open()
                .await
                .expect("global db should open for credential-remote seeding");
            global_db
                .upsert_code_project(
                    &target_project_id,
                    &target_root,
                    None,
                    Some(credential_remote_url),
                    None,
                )
                .await
                .expect("target project should accept credential-bearing remote upsert");
        }

        let (status, projects) = get_json(&agent, &format!("{}/api/projects", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(projects["status"], "ok");
        assert_eq!(
            projects["active_project_root"],
            fixture.project_root.display().to_string()
        );
        assert!(
            !projects.to_string().contains("sekret-token"),
            "project list response must not leak credential-bearing remote URL: {projects}"
        );
        let rows = projects["projects"]
            .as_array()
            .unwrap_or_else(|| panic!("expected project list array: {projects}"));
        let tree = projects["project_tree"]
            .as_array()
            .unwrap_or_else(|| panic!("project list should include compact tree: {projects}"));
        assert!(
            tree.iter().any(|group| {
                group["projects"].as_array().is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| entry["project_id"] == target_project_id)
                })
            }),
            "project tree should contain the target project id {target_project_id}: {projects}"
        );
        assert!(
            projects["summary"]["project_count"]
                .as_u64()
                .unwrap_or_default()
                >= 2,
            "project list should include summary counts: {projects}"
        );
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

        assert!(
            rows.iter()
                .any(|row| row["project_id"] == target_project_id),
            "target project id should be listed: {projects}"
        );
        let (status, context) = get_json(
            &agent,
            &format!("{}/api/projects/{target_project_id}", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert!(
            !context.to_string().contains("git_remote_url"),
            "project context should omit credential-bearing remote metadata field: {context}"
        );
        assert!(
            !context.to_string().contains("sekret-token"),
            "project context response must not leak the credential-bearing remote URL: {context}"
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
            .execute_write(
                "seed dashboard project fact fixture",
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
                "{}/api/projects/{}/plugins/holographic/curate/apply",
                fixture.base_url, target_project_id
            ),
            &serde_json::json!({ "ops": [] }),
        );
        assert_eq!(status, 405);
        assert_eq!(body["status"], "read_only_project");
    });
}

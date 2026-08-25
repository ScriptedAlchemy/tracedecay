use crate::dashboard_api_support::*;
use std::path::PathBuf;
use tracedecay_domain::FactId;

pub(crate) async fn setup_target_project(fixture: &DashboardFixture) -> (PathBuf, Arc<TraceDecay>) {
    let target_root = fixture
        ._tmp
        .path()
        .canonicalize()
        .expect("fixture root should canonicalize")
        .join("target-project");
    write_file(
        &target_root.join("src/lib.rs"),
        "pub fn target_fixture() -> &'static str { \"target\" }\n",
    );
    let target_project_id =
        ProjectId::new("dashboard_fixture_target_project").expect("target project identity");
    let target_cg = fixture
        .host_runtime
        .initialize_project_graph_with_id_for_test(&target_root, target_project_id)
        .await
        .expect("initialize retained target project");
    let target_cg = Arc::new(target_cg);
    fixture.project_graphs.register(target_cg.clone());
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
        fixture
            .host_runtime
            .upsert_code_project(
                &target_project_id,
                &target_root,
                None,
                Some(credential_remote_url),
                None,
            )
            .await
            .expect("target project should accept credential-bearing remote upsert");

        let (status, projects) = get_json(&agent, &format!("{}/api/projects", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(projects["domain_state"], "ready");
        let projects = &projects["payload"];
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
fn dashboard_projects_endpoint_does_not_launder_registry_read_failure() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent_with_timeout(std::time::Duration::from_secs(20));
        rusqlite::Connection::open(&fixture.global_db_path)
            .expect("open dashboard registry fixture")
            .execute_batch("DROP TABLE code_projects")
            .expect("break dashboard registry project reads");

        let (status, projects) = get_json(&agent, &format!("{}/api/projects", fixture.base_url));

        // The registry read failure is carried typed in the envelope: an
        // unknown domain state whose coverage names the exact failure, and a
        // payload that fabricates no project rows. Nothing is laundered into
        // a "ready" read.
        assert_eq!(status, 200, "{projects}");
        assert_eq!(projects["domain_state"], "unknown", "{projects}");
        assert!(
            projects["coverage"]["omission_reasons"][0]
                .as_str()
                .unwrap_or_default()
                .contains("code_projects"),
            "coverage must carry the registry read failure: {projects}"
        );
        let projects = &projects["payload"];
        assert_eq!(projects["status"], "registry_unavailable");
        assert!(
            projects["error"]
                .as_str()
                .unwrap_or_default()
                .contains("code_projects"),
            "payload must carry the registry read failure: {projects}"
        );
        assert_eq!(projects["summary"], serde_json::Value::Null);
        assert_eq!(projects["projects"], serde_json::Value::Null);
        assert_eq!(projects["project_tree"], serde_json::Value::Null);
        assert_eq!(projects["truncated"], serde_json::Value::Null);
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
        let target_fact_id = seed_dashboard_fact(
            &target_cg,
            "Target daemon project selector fact",
            tracedecay_domain::FactCategoryV1::Project,
            0.91,
            &["selector"],
            &[],
        )
        .await;
        target_cg
            .checkpoint()
            .await
            .expect("target project DB should checkpoint before dashboard reopen");
        drop(target_cg);

        let (active_status, active_payload) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/?q=selector&limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(active_status, 200);
        assert_eq!(
            active_payload["payload"]["holographic"]["facts"]
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
        let selected_facts = selected_payload["payload"]["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected selected project facts: {selected_payload}"));
        assert_eq!(selected_facts.len(), 1);
        let selected_fact_id = selected_facts[0]["fact_id"]
            .as_str()
            .unwrap_or_else(|| panic!("dashboard fact identity must be canonical text: {selected_payload}"));
        assert_eq!(selected_fact_id, target_fact_id.as_str());
        FactId::new(selected_fact_id.to_owned())
            .unwrap_or_else(|error| panic!("dashboard returned invalid canonical fact id: {error}"));
        assert_eq!(
            selected_facts[0]["content"],
            "Target daemon project selector fact"
        );
        assert_eq!(
            selected_payload["payload"]["holographic"]["graph"]["coverage"]["completeness"],
            "complete",
            "mounted selected-project graph must report explicit complete coverage: {selected_payload}"
        );
        let graph_nodes = selected_payload["payload"]["holographic"]["graph"]["nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("selected project graph nodes must be an array"));
        assert!(
            graph_nodes.iter().any(|node| {
                node["fact_id"].as_str() == Some(target_fact_id.as_str())
            }),
            "selected project graph must hydrate the exact target fact: {selected_payload}"
        );
        assert!(
            graph_nodes.iter().all(|node| {
                node.get("fact_id")
                    .is_none_or(|fact_id| fact_id.as_str().is_some())
            }),
            "graph fact identities must remain canonical strings: {selected_payload}"
        );
    });
}

#[test]
fn project_scoped_gateway_refuses_profile_owned_automation_skills() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent_with_timeout(std::time::Duration::from_secs(20));
        let (_target_root, target_cg) = setup_target_project(&fixture).await;
        let target_project_id = project_id(&target_cg);
        let active_project_id = fixture.host_runtime.project_id().as_str().to_owned();

        let (active_status, active_skills) = get_json(
            &agent,
            &format!("{}/api/automation/skills", fixture.base_url),
        );
        assert_eq!(
            active_status, 200,
            "profile-owned automation skills must remain mounted at their canonical route: {active_skills}"
        );
        assert!(active_skills["skills"].is_array(), "{active_skills}");

        for project_id in [active_project_id, target_project_id] {
            for tail in ["automation/skills", "automation/skills/repo-hygiene"] {
                let (status, refused) = get_json(
                    &agent,
                    &format!(
                        "{}/api/projects/{project_id}/{tail}",
                        fixture.base_url
                    ),
                );
                assert_eq!(
                    status, 404,
                    "project-qualified profile authority must be refused: {refused}"
                );
                assert_eq!(refused["status"], "not_project_scoped");
                assert_eq!(refused["project_id"], project_id);
                assert!(refused.get("skills").is_none());
            }
        }
    });
}

#[test]
fn project_scoped_gateway_reports_registry_read_failures_as_unavailable() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent_with_timeout(std::time::Duration::from_secs(20));
        let (_target_root, target_cg) = setup_target_project(&fixture).await;
        let target_project_id = project_id(&target_cg);
        drop(target_cg);
        rusqlite::Connection::open(&fixture.global_db_path)
            .expect("open dashboard registry fixture")
            .execute_batch("DROP TABLE project_aliases")
            .expect("break dashboard registry reads");

        let (context_status, context) = get_json(
            &agent,
            &format!("{}/api/projects/{target_project_id}", fixture.base_url),
        );
        // Registry read failures stay typed in the envelope: an unknown
        // domain state carrying the exact error, never a laundered "ready".
        assert_eq!(context_status, 200, "{context}");
        assert_eq!(context["domain_state"], "unknown", "{context}");
        assert_eq!(context["payload"]["status"], "registry_unavailable");
        assert!(
            context["payload"]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("project_aliases"),
            "context must carry the registry read failure: {context}"
        );

        let (gateway_status, gateway) = get_json(
            &agent,
            &format!(
                "{}/api/projects/{target_project_id}/plugins/holographic/",
                fixture.base_url
            ),
        );
        assert_eq!(gateway_status, 200, "{gateway}");
        assert_eq!(gateway["domain_state"], "unknown", "{gateway}");
        assert_eq!(gateway["payload"]["status"], "registry_unavailable");
    });
}

//! Session/response-handle artifact routing tests.

use super::*;

#[tokio::test]
async fn resolved_project_store_helpers_route_profile_sharded_session_artifacts() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(&project).unwrap();
    let profile = test_profile(&home);
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(&project, "proj_123")
        .unwrap();

    assert_path_eq(
        resolve_layout(&project, profile.data_dir())
            .unwrap()
            .sessions_db_path,
        profile_root.join("projects/proj_123/sessions.db"),
    );
    assert_path_eq(
        resolve_layout(&project, profile.data_dir())
            .unwrap()
            .lcm_payload_root,
        profile_root.join("projects/proj_123/lcm-payloads"),
    );
}

#[tokio::test]
async fn hermes_profile_like_directory_uses_user_profile_shard() {
    let dir = TempDir::new().unwrap();
    let hermes_home = dir.path().join(".hermes");
    let home = test_home(&dir);
    fs::create_dir_all(&hermes_home).unwrap();
    fs::write(
        hermes_home.join("config.yaml"),
        "memory:\n  provider: tracedecay\n",
    )
    .unwrap();
    let profile = test_profile(&home);
    let project_id = default_profile_project_id(&hermes_home);

    let expected = home
        .join(".tracedecay")
        .join(format!("projects/{project_id}/sessions.db"));
    assert_path_eq(
        resolve_layout(&hermes_home, profile.data_dir())
            .unwrap()
            .sessions_db_path,
        expected,
    );
}

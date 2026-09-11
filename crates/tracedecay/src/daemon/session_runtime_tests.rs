//! Session-runtime lifecycle assertions that reach daemon-private owners.
//!
//! The public-API journeys for temporal refresh scheduling, retained history
//! ingest, and session sync live in `tests/session_suite/session_runtime`;
//! only the test that drives `project_server_lifecycle` directly stays here.

use tempfile::TempDir;
use tracedecay_session_runtime::StoreOwnerKey;
use tracedecay_sessions::admission::HostAdmissionScope;

use crate::test_support::host_admission::HostAdmissionTestRuntimeV1;

#[tokio::test]
async fn evicted_project_owner_releases_temporal_scheduler() {
    let temp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project(
        temp.path().join("evict-owner-profile"),
        temp.path().join("evict-owner-project"),
        tracedecay_domain::ProjectId::new("project.refresh-evict-owner").unwrap(),
    )
    .await
    .unwrap();
    let database = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .expect("registered temporal test database");
    let owner = StoreOwnerKey {
        profile_root: temp.path().to_path_buf(),
        global_db_path: temp.path().join("global.db"),
        project_id: Some("project.evict-owner".to_string()),
        store_root: temp.path().join("store"),
        graph_db_path: temp.path().join("store/graph.db"),
    };
    let administration = crate::daemon::branch_admin::StoreAdministration::default();
    let registry = administration.session_temporal_refresh_schedulers();
    registry.ensure_project(owner.clone(), database).await;
    assert_eq!(registry.project_worker_count().await, 1);

    crate::daemon::project_server_lifecycle::retire_evicted_project_owner(
        &administration,
        owner.clone(),
        Vec::new(),
        None,
    )
    .await;

    assert_eq!(
        registry.project_worker_count().await,
        0,
        "project-server owner eviction must release the temporal scheduler"
    );
    assert!(
        registry.project_state(&owner).await.is_none(),
        "retired owner must not retain a temporal scheduler entry"
    );
}

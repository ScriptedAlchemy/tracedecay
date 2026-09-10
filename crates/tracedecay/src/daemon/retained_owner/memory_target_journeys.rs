use std::path::Path;
use std::sync::Arc;

use tracedecay_contracts::RetainedSurfaceExecutionErrorV1;
use tracedecay_contracts::retained_surfaces::{MemoryScopeV1, RetainedProjectSelectorV1};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_store::StoreShardScopeV1;
use tracedecay_store_runtime::retained_memory::{
    MemoryTargetAccessV1, RetainedMemoryTargetAuthorityV1,
};

use crate::daemon::retained_owner::open_project_retained_memory_target;
use crate::tracedecay::TraceDecay;

fn open_options(profile_root: &Path) -> crate::tracedecay::TraceDecayOpenOptions {
    crate::tracedecay::TraceDecayOpenOptions {
        global_db_path: Some(profile_root.join("global.db")),
        profile_root: Some(profile_root.to_path_buf()),
    }
}

fn project_id(cg: &TraceDecay) -> ProjectId {
    let FactOwnerV1::Project { project_id } = cg.project_memory_owner().unwrap() else {
        panic!("fixture must have a project memory owner");
    };
    project_id
}

async fn register_project(cg: &TraceDecay, project_id: &ProjectId, project_root: &Path) {
    cg.profile_database()
        .upsert_code_project(project_id.as_str(), project_root, None, None, Some("main"))
        .await
        .unwrap();
}

async fn project_pair() -> (
    tempfile::TempDir,
    TraceDecay,
    TraceDecay,
    Arc<crate::test_support::host_admission::HostAdmissionTestRuntimeV1>,
) {
    let tmp = tempfile::tempdir().unwrap();
    // Register the same canonical paths that retained-target lookup uses.
    let fixture_root = tmp.path().canonicalize().unwrap();
    let profile_root = fixture_root.join("profile");
    let active_root = fixture_root.join("active");
    std::fs::create_dir_all(&active_root).unwrap();
    let active = TraceDecay::init_with_options(&active_root, open_options(&profile_root))
        .await
        .unwrap();
    let runtime = active.test_runtime_for_test().unwrap();
    let selected_root = fixture_root.join("selected");
    std::fs::create_dir_all(&selected_root).unwrap();
    let selected_id = ProjectId::new(
        tracedecay_runtime_core::storage::default_profile_project_id(&selected_root),
    )
    .unwrap();
    let sibling = Arc::new(
        runtime
            .sibling_project(&selected_root, selected_id)
            .await
            .unwrap(),
    );
    let selected = sibling
        .initialize_project_graph_for_test(&selected_root, open_options(&profile_root))
        .await
        .unwrap();
    for graph in [&active, &selected] {
        register_project(&active, &project_id(graph), graph.project_root()).await;
    }
    (tmp, active, selected, sibling)
}

fn selector(project_id: ProjectId) -> RetainedProjectSelectorV1 {
    RetainedProjectSelectorV1 { project_id }
}

#[tokio::test]
async fn selected_project_opens_its_exact_read_only_store_not_the_active_store() {
    let (_tmp, active, selected, _sibling) = project_pair().await;
    let active_id = project_id(&active);
    let selected_id = project_id(&selected);

    let active_target = open_project_retained_memory_target(
        &active,
        active.project_root(),
        &active_id,
        Some(MemoryScopeV1::Project),
        None,
        MemoryTargetAccessV1::Read,
    )
    .await
    .unwrap();
    let selected_selector = selector(selected_id.clone());
    let selected_target = open_project_retained_memory_target(
        &active,
        active.project_root(),
        &active_id,
        Some(MemoryScopeV1::Project),
        Some(&selected_selector),
        MemoryTargetAccessV1::Read,
    )
    .await
    .unwrap();

    assert!(!active_target.database().is_writable());
    assert!(!selected_target.database().is_writable());
    assert_eq!(
        selected_target.owner(),
        &FactOwnerV1::Project {
            project_id: selected_id.clone(),
        }
    );
    assert!(!std::ptr::eq(
        active_target.database(),
        selected_target.database()
    ));
    assert!(matches!(
        &selected_target.database().registered_binding().shard_id.scope,
        StoreShardScopeV1::Project { project_id } if project_id == &selected_id
    ));
}

#[tokio::test]
async fn missing_unenrolled_and_write_selected_targets_share_one_denial() {
    let (tmp, active, selected, _sibling) = project_pair().await;
    let active_id = project_id(&active);
    let selected_selector = selector(project_id(&selected));
    let missing_selector = selector(ProjectId::new("proj_missing").unwrap());
    let unenrolled_root = tmp.path().join("unenrolled");
    std::fs::create_dir_all(&unenrolled_root).unwrap();
    let unenrolled_id = ProjectId::new("proj_unenrolled").unwrap();
    register_project(&active, &unenrolled_id, &unenrolled_root).await;
    let unenrolled_selector = selector(unenrolled_id);

    for (selector, access) in [
        (&missing_selector, MemoryTargetAccessV1::Read),
        (&unenrolled_selector, MemoryTargetAccessV1::Read),
        (&selected_selector, MemoryTargetAccessV1::Write),
    ] {
        let error = open_project_retained_memory_target(
            &active,
            active.project_root(),
            &active_id,
            Some(MemoryScopeV1::Project),
            Some(selector),
            access,
        )
        .await
        .err()
        .expect("target must be denied");
        assert!(matches!(
            error,
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        ));
    }
}

#[tokio::test]
async fn assembled_memory_authority_tracks_swapped_graph_identity() {
    let (_tmp, active, selected, _sibling) = project_pair().await;
    let active_id = project_id(&active);
    let active_root = active.project_root().to_path_buf();
    let selected_root = selected.project_root().to_path_buf();
    let lock = Arc::new(tokio::sync::RwLock::new(Arc::new(active)));
    let before = super::live_retained_memory_authority(&lock, &active_id, &active_root)
        .await
        .expect("active graph must resolve a served identity");
    assert_eq!(before.served_project_root, active_root);
    *lock.write().await = Arc::new(selected);
    let after = super::live_retained_memory_authority(&lock, &active_id, &active_root)
        .await
        .expect("swapped graph must resolve a served identity");
    assert_eq!(after.served_project_root, selected_root);
    let error = tracedecay_store_runtime::retained_memory::open_project_retained_memory_target(
        &after,
        &active_root,
        &active_id,
        Some(MemoryScopeV1::Project),
        None,
        MemoryTargetAccessV1::Read,
    )
    .await
    .err()
    .expect("open must deny against the swapped served identity");
    assert!(matches!(
        error,
        RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
    ));
}

#[tokio::test]
async fn same_project_open_denies_when_store_identity_disagrees() {
    let (_tmp, active, selected, _sibling) = project_pair().await;
    let active_id = project_id(&active);
    let selected_id = project_id(&selected);
    let authority = RetainedMemoryTargetAuthorityV1 {
        registry: active.retained_store_runtime_registry(),
        profile_database: active.profile_database().clone(),
        project_root: active.project_root().to_path_buf(),
        project_id: active_id.clone(),
        store_layout_project_id: selected_id,
        served_project_root: active.project_root().to_path_buf(),
        graph_read_only: false,
    };

    let error = tracedecay_store_runtime::retained_memory::open_project_retained_memory_target(
        &authority,
        active.project_root(),
        &active_id,
        Some(MemoryScopeV1::Project),
        None,
        MemoryTargetAccessV1::Read,
    )
    .await
    .err()
    .expect("store identity drift must deny");
    assert!(matches!(
        error,
        RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
    ));
}

//! Moved/renamed checkout journeys for the project registry (Plan 16:
//! project moves preserve identity). Ported from the salvaged
//! identity-across-moves work and adapted to the current registry contract.

use std::path::{Path, PathBuf};

use crate::RegisteredGlobalDb;
use crate::tests::harness::{RegisteredGlobalDbHarness, RegisteredGlobalDbTestRuntime};

async fn register(db: &RegisteredGlobalDb, project_id: &str, root: &Path) {
    let record = db
        .upsert_code_project(project_id, root, None, None, Some("main"))
        .await
        .expect("project root admission");
    assert_eq!(record.project_id, project_id);
}

fn project_roots(harness: &RegisteredGlobalDbHarness, label: &str) -> (PathBuf, PathBuf) {
    let storage_root = harness
        .registered
        .db_path()
        .parent()
        .expect("registered database storage root");
    let original = storage_root.join(format!("{label}-original"));
    let replacement = storage_root.join(format!("{label}-replacement"));
    std::fs::create_dir_all(&original).expect("create original project root");
    (original, replacement)
}

async fn project_id_by_alias(db: &RegisteredGlobalDb, alias: &Path) -> Option<String> {
    db.project_registry_context_by_alias(alias)
        .await
        .expect("resolve project registry alias")
        .map(|context| context.project.project_id)
}

#[tokio::test]
async fn moved_checkout_old_root_alias_resolves_to_same_project() {
    let harness = RegisteredGlobalDbHarness::open("moved-checkout-identity").await;
    let (original, replacement) = project_roots(&harness, "moved");
    register(&harness.registered, "project-moved", &original).await;

    std::fs::rename(&original, &replacement).expect("move project root");
    register(&harness.registered, "project-moved", &replacement).await;

    let record = harness
        .registered
        .get_code_project("project-moved")
        .await
        .expect("registry read for the moved project should not fault")
        .expect("moved project remains registered");
    assert_eq!(
        record.canonical_root,
        super::canonical_project_path(&replacement)
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(
        project_id_by_alias(&harness.registered, &replacement).await,
        Some("project-moved".to_owned()),
        "current root must resolve after the move"
    );
    assert_eq!(
        project_id_by_alias(&harness.registered, &original).await,
        Some("project-moved".to_owned()),
        "former root must keep resolving to the same project after the move"
    );
}

#[tokio::test]
async fn failed_reregistration_rolls_back_without_leaking_replacement_alias() {
    let harness = RegisteredGlobalDbHarness::open("moved-checkout-rollback").await;
    let (original, replacement) = project_roots(&harness, "rollback");
    std::fs::create_dir_all(&replacement).expect("create replacement project root");
    register(&harness.registered, "project-rollback", &original).await;

    harness
        .registered
        .writer_connection()
        .expect("registered writer")
        .execute_batch(
            "CREATE TRIGGER fail_code_project_update
             BEFORE UPDATE ON code_projects
             BEGIN
               SELECT RAISE(ABORT, 'injected code project write failure');
             END;",
        )
        .await
        .expect("inject project registry write failure");

    let failure = harness
        .registered
        .upsert_code_project("project-rollback", &replacement, None, None, None)
        .await
        .expect_err("injected project update failure must not report success");
    assert!(
        failure.is_database_error(),
        "an injected write fault must surface as a database fault, not as an \
         admission refusal or a reset demand: {failure:?}"
    );
    assert!(
        failure.to_string().contains("upsert code project"),
        "the database fault must name the operation it failed: {failure}"
    );

    harness
        .registered
        .writer_connection()
        .expect("registered writer")
        .execute_batch("DROP TRIGGER fail_code_project_update")
        .await
        .expect("remove project registry write fault");

    let record = harness
        .registered
        .get_code_project("project-rollback")
        .await
        .expect("registry read for the rolled-back project should not fault")
        .expect("original project remains registered");
    assert_eq!(
        record.canonical_root,
        super::canonical_project_path(&original)
            .to_string_lossy()
            .into_owned(),
        "failed re-registration must not repoint the canonical root"
    );
    assert_eq!(
        project_id_by_alias(&harness.registered, &original).await,
        Some("project-rollback".to_owned())
    );
    assert_eq!(
        project_id_by_alias(&harness.registered, &replacement).await,
        None,
        "failed project upsert leaked its replacement root alias"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn moved_project_alias_survives_runtime_restart_and_missing_symlink_tail() {
    let temporary = tempfile::tempdir().expect("temporary project registry");
    let profile_root = temporary.path().join("profile");
    let physical_parent = temporary.path().join("physical");
    let alias_parent = temporary.path().join("alias");
    std::fs::create_dir_all(&physical_parent).expect("create physical project parent");
    std::os::unix::fs::symlink(&physical_parent, &alias_parent)
        .expect("create project parent alias");
    let old_physical_root = physical_parent.join("before-move");
    let old_alias_root = alias_parent.join("before-move");
    let current_root = physical_parent.join("after-move");
    std::fs::create_dir_all(&old_physical_root).expect("create old project root");
    // Captured while the old root still exists: the exact key the registry
    // stored for it at registration time.
    let canonical_old_key = super::project_path_alias_key(&old_physical_root);

    let runtime = RegisteredGlobalDbTestRuntime::profile(&profile_root)
        .await
        .expect("open first project registry runtime");
    register(
        runtime.profile_database(),
        "stable-project",
        &old_alias_root,
    )
    .await;
    std::fs::rename(&old_physical_root, &current_root).expect("move project root");
    register(runtime.profile_database(), "stable-project", &current_root).await;

    assert_eq!(
        super::project_path_alias_key(&old_alias_root),
        canonical_old_key,
        "missing-tail symlink alias must keep canonicalizing to the retained key"
    );
    assert_eq!(
        project_id_by_alias(runtime.profile_database(), &old_alias_root).await,
        Some("stable-project".to_owned()),
        "old symlink-aliased root missing before restart"
    );
    drop(runtime);

    let restarted = RegisteredGlobalDbTestRuntime::profile(&profile_root)
        .await
        .expect("restart project registry runtime");
    let old_project = project_id_by_alias(restarted.profile_database(), &old_alias_root)
        .await
        .expect("old missing-tail alias retained after restart");
    let current_project = project_id_by_alias(restarted.profile_database(), &current_root)
        .await
        .expect("current root registered after restart");
    assert_eq!(old_project, "stable-project");
    assert_eq!(old_project, current_project);
}

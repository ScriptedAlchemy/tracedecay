use std::path::{Path, PathBuf};

use crate::RegisteredGlobalDb;
use crate::tests::harness::{RegisteredGlobalDbHarness, RegisteredGlobalDbTestRuntime};

async fn register(db: &RegisteredGlobalDb, project_id: &str, root: &Path) {
    db.upsert_code_project(project_id, root, None, None, Some("main"))
        .await
        .expect("project registry write")
        .expect("project root admission");
}

async fn assert_registration_unchanged(
    db: &RegisteredGlobalDb,
    project_id: &str,
    original_root: &Path,
    rejected_root: &Path,
) {
    let context = db
        .project_registry_context_by_id(project_id)
        .await
        .expect("read project registry")
        .expect("original project remains registered");
    assert_eq!(
        context.project.canonical_root,
        original_root.to_string_lossy()
    );
    assert!(
        db.project_registry_context_by_alias(original_root)
            .await
            .expect("resolve original alias")
            .is_some()
    );
    assert!(
        db.project_registry_context_by_alias(rejected_root)
            .await
            .expect("resolve rejected alias")
            .is_none(),
        "failed project upsert leaked its replacement root alias"
    );
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
    std::fs::create_dir_all(&replacement).expect("create replacement project root");
    (original, replacement)
}

#[tokio::test]
async fn code_project_upsert_propagates_begin_failure_without_partial_registration() {
    let harness = RegisteredGlobalDbHarness::open("project-upsert-begin-failure").await;
    let (original, replacement) = project_roots(&harness, "begin");
    register(&harness.registered, "project-begin", &original).await;

    let database_path = harness.registered.db_path().to_path_buf();
    let moved_database_path = database_path.with_extension("begin-fault");
    std::fs::rename(&database_path, &moved_database_path)
        .expect("inject transaction begin identity failure");
    let error = harness
        .registered
        .upsert_code_project("project-begin", &replacement, None, None, None)
        .await
        .expect_err("moved database must reject transaction begin");
    assert!(error.to_string().contains("upsert code project"), "{error}");

    std::fs::rename(&moved_database_path, &database_path)
        .expect("restore transaction begin database");
    assert_registration_unchanged(
        &harness.registered,
        "project-begin",
        &original,
        &replacement,
    )
    .await;
}

#[tokio::test]
async fn code_project_upsert_propagates_read_failure_without_partial_registration() {
    let harness = RegisteredGlobalDbHarness::open("project-upsert-read-failure").await;
    let (original, replacement) = project_roots(&harness, "read");
    register(&harness.registered, "project-read", &original).await;
    harness
        .registered
        .writer_connection()
        .expect("registered writer")
        .execute_batch("ALTER TABLE code_projects RENAME TO code_projects_fault")
        .await
        .expect("inject project registry read failure");

    let error = harness
        .registered
        .upsert_code_project("project-read", &replacement, None, None, None)
        .await
        .expect_err("missing code_projects table must propagate");
    assert!(error.to_string().contains("upsert code project"), "{error}");

    harness
        .registered
        .writer_connection()
        .expect("registered writer")
        .execute_batch("ALTER TABLE code_projects_fault RENAME TO code_projects")
        .await
        .expect("restore project registry table");
    assert_registration_unchanged(&harness.registered, "project-read", &original, &replacement)
        .await;
}

#[tokio::test]
async fn code_project_upsert_rolls_back_injected_write_failure() {
    let harness = RegisteredGlobalDbHarness::open("project-upsert-write-failure").await;
    let (original, replacement) = project_roots(&harness, "write");
    register(&harness.registered, "project-write", &original).await;
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

    let error = harness
        .registered
        .upsert_code_project("project-write", &replacement, None, None, None)
        .await
        .expect_err("injected project update failure must propagate");
    assert!(
        error
            .to_string()
            .contains("injected code project write failure"),
        "{error}"
    );

    harness
        .registered
        .writer_connection()
        .expect("registered writer")
        .execute_batch("DROP TRIGGER fail_code_project_update")
        .await
        .expect("remove project registry write fault");
    assert_registration_unchanged(
        &harness.registered,
        "project-write",
        &original,
        &replacement,
    )
    .await;
}

#[tokio::test]
async fn code_project_upsert_rolls_back_injected_commit_failure() {
    let harness = RegisteredGlobalDbHarness::open("project-upsert-commit-failure").await;
    let (original, replacement) = project_roots(&harness, "commit");
    register(&harness.registered, "project-commit", &original).await;
    harness
        .registered
        .writer_connection()
        .expect("registered writer")
        .execute_batch(
            "CREATE TABLE project_upsert_commit_parent (
               id TEXT PRIMARY KEY
             );
             CREATE TABLE project_upsert_commit_child (
               parent_id TEXT NOT NULL,
               FOREIGN KEY (parent_id) REFERENCES project_upsert_commit_parent(id)
                 DEFERRABLE INITIALLY DEFERRED
             );
             CREATE TRIGGER fail_code_project_commit
             AFTER UPDATE ON code_projects
             BEGIN
               INSERT INTO project_upsert_commit_child(parent_id) VALUES ('missing-parent');
             END;",
        )
        .await
        .expect("inject deferred project registry commit failure");

    let error = harness
        .registered
        .upsert_code_project("project-commit", &replacement, None, None, None)
        .await
        .expect_err("deferred commit failure must propagate");
    assert!(
        error.to_string().contains("FOREIGN KEY constraint failed"),
        "{error}"
    );

    harness
        .registered
        .writer_connection()
        .expect("registered writer")
        .execute_batch(
            "DROP TRIGGER fail_code_project_commit;
             DROP TABLE project_upsert_commit_child;
             DROP TABLE project_upsert_commit_parent;",
        )
        .await
        .expect("remove project registry commit fault");
    assert_registration_unchanged(
        &harness.registered,
        "project-commit",
        &original,
        &replacement,
    )
    .await;
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
    let snapshot = runtime
        .profile_database()
        .read_snapshot()
        .await
        .expect("inspect retained aliases");
    let mut rows = snapshot
        .query(
            "SELECT alias_path FROM project_aliases ORDER BY alias_path",
            (),
        )
        .await
        .expect("query retained aliases");
    let mut retained_aliases = Vec::new();
    while let Some(row) = rows.next().await.expect("read retained alias") {
        retained_aliases.push(row.get::<String>(0).expect("decode retained alias"));
    }
    assert!(
        retained_aliases.contains(&old_physical_root.to_string_lossy().into_owned()),
        "old physical alias missing before restart: {retained_aliases:?}"
    );
    drop(runtime);

    let restarted = RegisteredGlobalDbTestRuntime::profile(&profile_root)
        .await
        .expect("restart project registry runtime");
    assert_eq!(
        crate::project_registry::project_path_alias_key(&old_alias_root),
        old_physical_root.to_string_lossy()
    );
    let restarted_snapshot = restarted
        .profile_database()
        .read_snapshot()
        .await
        .expect("inspect aliases after restart");
    let mut restarted_rows = restarted_snapshot
        .query(
            "SELECT alias_path FROM project_aliases ORDER BY alias_path",
            (),
        )
        .await
        .expect("query aliases after restart");
    let mut restarted_aliases = Vec::new();
    while let Some(row) = restarted_rows
        .next()
        .await
        .expect("read alias after restart")
    {
        restarted_aliases.push(row.get::<String>(0).expect("decode alias after restart"));
    }
    assert!(
        restarted_aliases.contains(&old_physical_root.to_string_lossy().into_owned()),
        "old physical alias missing after restart: {restarted_aliases:?}"
    );
    let old_context = restarted
        .profile_database()
        .project_registry_context_by_alias(&old_alias_root)
        .await
        .expect("resolve old missing-tail alias after restart")
        .expect("old missing-tail alias retained");
    let current_context = restarted
        .profile_database()
        .project_registry_context_by_alias(&current_root)
        .await
        .expect("resolve current root after restart")
        .expect("current root registered");
    assert_eq!(old_context.project.project_id, "stable-project");
    assert_eq!(
        old_context.project.project_id,
        current_context.project.project_id
    );
}

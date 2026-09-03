//! Moved/renamed checkout journeys for the project registry. A moved project
//! keeps its identity and stays resolvable from its former root.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use tracedecay_runtime_core::db::engine::{
    Executor, IntoParams, QueryExecutor, Result as EngineResult, Rows, TestConnection, params,
};

use crate::RegisteredGlobalDb;
use crate::tests::harness::{RegisteredGlobalDbHarness, RegisteredGlobalDbTestRuntime};

struct RegistryQueryPlans {
    recent: Vec<String>,
    git_common_dir: Vec<String>,
    canonical_root: Vec<String>,
}

struct CountingQuery<'a> {
    inner: &'a TestConnection,
    statements: Cell<usize>,
}

impl QueryExecutor for CountingQuery<'_> {
    async fn query<P>(&self, sql: &str, params: P) -> EngineResult<Rows>
    where
        P: IntoParams,
    {
        self.statements.set(self.statements.get() + 1);
        self.inner.query(sql, params).await
    }
}

async fn large_registry_fixture() -> (tempfile::TempDir, TestConnection) {
    let directory = tempfile::tempdir().expect("project registry fixture");
    let connection = TestConnection::open(&directory.path().join("global.db"));
    connection
        .execute_batch(
            "CREATE TABLE code_projects (
                project_id TEXT PRIMARY KEY,
                canonical_root TEXT NOT NULL,
                display_root TEXT NOT NULL,
                primary_root_platform TEXT,
                primary_root_bytes BLOB,
                primary_root_last_seen_at INTEGER,
                git_common_dir TEXT,
                git_remote_url TEXT,
                default_branch TEXT,
                created_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL
             );
             CREATE TABLE project_aliases (
                alias_path TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                last_seen_at INTEGER NOT NULL
             );
             CREATE INDEX idx_project_aliases_project_id
                ON project_aliases(project_id);",
        )
        .await
        .expect("create project registry fixture schema");
    connection
        .execute(
            "WITH digits(value) AS (
                 VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9)
             ),
             sequence(value) AS (
                 SELECT ones.value
                      + tens.value * 10
                      + hundreds.value * 100
                      + thousands.value * 1000
                      + 1
                 FROM digits ones
                 CROSS JOIN digits tens
                 CROSS JOIN digits hundreds
                 CROSS JOIN digits thousands
             )
             INSERT INTO code_projects (
                 project_id, canonical_root, display_root,
                 primary_root_platform, primary_root_bytes,
                 primary_root_last_seen_at, git_common_dir, git_remote_url,
                 default_branch, created_at, last_seen_at
             )
             SELECT printf('project-%05d', value),
                    printf('/fixture/project-%05d', value),
                    printf('/fixture/project-%05d', value),
                    ?1,
                    CAST(printf('/fixture/project-%05d', value) AS BLOB),
                    10001 - value,
                    NULL,
                    NULL,
                    'main',
                    value,
                    10001 - value
             FROM sequence",
            params![super::native_project_path_platform()],
        )
        .await
        .expect("seed 10k code projects");
    connection
        .execute(
            "INSERT INTO project_aliases(alias_path, project_id, last_seen_at)
             SELECT canonical_root, project_id, last_seen_at
             FROM code_projects",
            (),
        )
        .await
        .expect("seed current project aliases");
    (directory, connection)
}

async fn explain(
    db: &RegisteredGlobalDb,
    sql: &str,
    params: impl tracedecay_runtime_core::db::engine::IntoParams,
) -> Vec<String> {
    let snapshot = db.read_snapshot().await.expect("registry read snapshot");
    let mut rows = snapshot
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)
        .await
        .expect("explain registry query");
    let mut plan = Vec::new();
    while let Some(row) = rows.next().await.expect("read registry query plan") {
        plan.push(row.get::<String>(3).expect("query plan detail"));
    }
    plan
}

async fn registry_query_plans(db: &RegisteredGlobalDb) -> RegistryQueryPlans {
    RegistryQueryPlans {
        recent: explain(
            db,
            "SELECT project_id, canonical_root
             FROM code_projects
             ORDER BY last_seen_at DESC, project_id
             LIMIT ?1",
            params![20_i64],
        )
        .await,
        git_common_dir: explain(
            db,
            "SELECT project_id
             FROM code_projects
             WHERE git_common_dir = ?1",
            params!["/repo/.git"],
        )
        .await,
        canonical_root: explain(
            db,
            "SELECT project_id
             FROM code_projects
             WHERE canonical_root = ?1 AND project_id != ?2",
            params!["/repo", "project-a"],
        )
        .await,
    }
}

fn assert_plan_uses(plan: &[String], index: &str) {
    assert!(
        plan.iter().any(|detail| detail.contains(index)),
        "query must use {index}:\n{}",
        plan.join("\n")
    );
}

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

#[tokio::test]
async fn project_registry_indexes_migrate_on_reopen_and_cover_actual_read_shapes() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-read-index-migration").await;
    harness
        .registered
        .writer_connection()
        .expect("registered writer")
        .execute_batch(
            "DROP INDEX IF EXISTS idx_code_projects_last_seen_project;
             DROP INDEX IF EXISTS idx_code_projects_git_common_dir;
             DROP INDEX IF EXISTS idx_code_projects_canonical_root_project;",
        )
        .await
        .expect("remove project registry read indexes");

    let pre = registry_query_plans(&harness.registered).await;
    for (name, plan) in [
        ("idx_code_projects_last_seen_project", &pre.recent),
        ("idx_code_projects_git_common_dir", &pre.git_common_dir),
        (
            "idx_code_projects_canonical_root_project",
            &pre.canonical_root,
        ),
    ] {
        assert!(
            plan.iter().all(|detail| !detail.contains(name)),
            "pre-migration plan unexpectedly used {name}:\n{}",
            plan.join("\n")
        );
    }
    assert!(
        pre.recent
            .iter()
            .any(|detail| detail.contains("USE TEMP B-TREE FOR ORDER BY")),
        "pre-migration recent-project query must expose its sorting regression:\n{}",
        pre.recent.join("\n")
    );

    let harness = harness.restart().await;
    let post = registry_query_plans(&harness.registered).await;
    assert_plan_uses(&post.recent, "idx_code_projects_last_seen_project");
    assert_plan_uses(&post.git_common_dir, "idx_code_projects_git_common_dir");
    assert_plan_uses(
        &post.canonical_root,
        "idx_code_projects_canonical_root_project",
    );
}

#[tokio::test]
async fn listing_ten_thousand_projects_uses_one_set_based_statement() {
    let (_directory, connection) = large_registry_fixture().await;
    let query = CountingQuery {
        inner: &connection,
        statements: Cell::new(0),
    };

    let paths = super::list_code_project_paths_from(&query, 10_000)
        .await
        .expect("list project paths");

    assert_eq!(paths.len(), 10_000);
    assert_eq!(
        paths.first(),
        Some(&PathBuf::from("/fixture/project-00001"))
    );
    assert_eq!(paths.last(), Some(&PathBuf::from("/fixture/project-10000")));
    assert_eq!(
        query.statements.get(),
        1,
        "listing cost must not grow with the number of registered projects"
    );
}

#[tokio::test]
async fn current_common_dir_alias_does_not_hide_a_stale_primary_root_alias() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-stale-primary-alias").await;
    let storage_root = harness
        .registered
        .db_path()
        .parent()
        .expect("registered database storage root");
    let root = storage_root.join("stale-primary-root");
    let common_dir = storage_root.join("stale-primary-common-dir");
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::create_dir_all(&common_dir).expect("create git common dir");
    harness
        .registered
        .upsert_code_project(
            "project-stale-primary",
            &root,
            Some(&common_dir),
            None,
            Some("main"),
        )
        .await
        .expect("register project with common dir");
    harness
        .registered
        .writer_connection()
        .expect("registered writer")
        .execute(
            "UPDATE project_aliases
             SET last_seen_at = last_seen_at - 1
             WHERE alias_path = ?1",
            params![super::project_path_alias_key(&root)],
        )
        .await
        .expect("stale only the primary root alias");

    assert!(
        harness
            .registered
            .try_list_code_project_paths(10)
            .await
            .expect("list project paths")
            .is_empty(),
        "a current common-dir alias must not substitute for stale exact root identity"
    );
}

#[tokio::test]
async fn linked_worktree_refresh_lists_the_current_root_for_shared_identity() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-linked-worktree").await;
    let storage_root = harness
        .registered
        .db_path()
        .parent()
        .expect("registered database storage root");
    let primary = storage_root.join("primary-worktree");
    let linked = storage_root.join("linked-worktree");
    let common_dir = storage_root.join("shared-git-common-dir");
    for path in [&primary, &linked, &common_dir] {
        std::fs::create_dir_all(path).expect("create linked-worktree fixture path");
    }
    for root in [&primary, &linked] {
        harness
            .registered
            .upsert_code_project(
                "project-shared-repository",
                root,
                Some(&common_dir),
                None,
                Some("main"),
            )
            .await
            .expect("refresh shared project identity");
    }

    assert_eq!(
        harness
            .registered
            .try_list_code_project_paths(10)
            .await
            .expect("list linked-worktree project paths"),
        vec![super::canonical_project_path(&linked)]
    );
}

#[tokio::test]
async fn project_path_listing_orders_recency_then_project_id_deterministically() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-deterministic-list").await;
    let storage_root = harness
        .registered
        .db_path()
        .parent()
        .expect("registered database storage root");
    let mut roots = Vec::new();
    for project_id in ["project-b", "project-a", "project-c"] {
        let root = storage_root.join(project_id);
        std::fs::create_dir_all(&root).expect("create ordered project root");
        register(&harness.registered, project_id, &root).await;
        roots.push((project_id, root));
    }
    for (project_id, timestamp) in [
        ("project-a", 100_i64),
        ("project-b", 100_i64),
        ("project-c", 200_i64),
    ] {
        harness
            .registered
            .writer_connection()
            .expect("registered writer")
            .execute(
                "UPDATE code_projects
                 SET last_seen_at = ?2, primary_root_last_seen_at = ?2
                 WHERE project_id = ?1",
                params![project_id, timestamp],
            )
            .await
            .expect("set project recency");
        harness
            .registered
            .writer_connection()
            .expect("registered writer")
            .execute(
                "UPDATE project_aliases SET last_seen_at = ?2 WHERE project_id = ?1",
                params![project_id, timestamp],
            )
            .await
            .expect("set alias recency");
    }

    assert_eq!(
        harness
            .registered
            .try_list_code_project_paths(3)
            .await
            .expect("list ordered project paths"),
        vec![
            super::canonical_project_path(&roots[2].1),
            super::canonical_project_path(&roots[1].1),
            super::canonical_project_path(&roots[0].1),
        ]
    );
}

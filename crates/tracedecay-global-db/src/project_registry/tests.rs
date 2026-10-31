//! Moved/renamed checkout journeys for the project registry. A moved project
//! keeps its identity and stays resolvable from its former root.

use std::cell::Cell;
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

use tracedecay_runtime_core::db::engine::{
    Executor, IntoParams, QueryExecutor, Result as EngineResult, Rows, TestConnection, params,
};
use tracedecay_runtime_core::path_safety::plain_git_args;

use crate::RegisteredGlobalDb;
use crate::tests::harness::RegisteredGlobalDbHarness;
#[cfg(unix)]
use crate::tests::harness::RegisteredGlobalDbTestRuntime;

#[cfg(unix)]
#[test]
fn ephemeral_admission_resolves_missing_profiles_through_symlinked_parents() {
    let temporary = tempfile::tempdir().unwrap();
    let alias = temporary.path().join("alias");
    std::os::unix::fs::symlink(temporary.path(), &alias).unwrap();
    let profile = alias.join("not-created/profile");
    assert!(!profile.exists());
    assert!(super::ephemeral_root_rejection(temporary.path(), &profile).is_none());

    let outside = temporary.path().join("outside");
    std::os::unix::fs::symlink("/", &outside).unwrap();
    let profile = outside
        .join(temporary.path().file_name().unwrap())
        .join("not-created/profile");
    assert!(!profile.exists());
    assert!(super::ephemeral_root_rejection(temporary.path(), &profile).is_some());
}

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

/// The registered root every fixture project lives under, spelled the way
/// `upsert_code_project` stores it on this host: a bare `/fixture` literal is
/// not absolute on Windows, and the registry keys its rows by the canonical
/// (`\\?\`-prefixed) spelling there.
fn large_registry_root() -> PathBuf {
    let posix = "/fixture";
    let host = if cfg!(windows) {
        PathBuf::from(format!("C:{}", posix.replace('/', "\\")))
    } else {
        PathBuf::from(posix)
    };
    super::canonical_project_path(&host)
}

async fn large_registry_fixture() -> (tempfile::TempDir, TestConnection) {
    let directory = tempfile::tempdir().expect("project registry fixture");
    let connection = TestConnection::open(&directory.path().join("global.db"));
    // Text columns take the project root spelled as text; the primary-root
    // evidence column takes it in the host's native encoding (UTF-8 bytes on
    // Unix, UTF-16LE on Windows). The constant prefix is encoded once by the
    // production encoder and the five ASCII digits are widened in SQL (`||`
    // yields text even for blob operands, hence the outer cast), so the seed
    // stays one set-based statement on every host.
    let root_prefix = format!(
        "{}{}project-",
        large_registry_root().display(),
        std::path::MAIN_SEPARATOR
    );
    let native_prefix = super::encode_native_project_path(Path::new(&root_prefix));
    let native_pad: &[u8] = if cfg!(windows) { &[0] } else { &[] };
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
                    ?2 || printf('%05d', value),
                    ?2 || printf('%05d', value),
                    ?1,
                    CAST(?3 || CAST(substr(printf('%05d', value), 1, 1) AS BLOB) || ?4
                            || CAST(substr(printf('%05d', value), 2, 1) AS BLOB) || ?4
                            || CAST(substr(printf('%05d', value), 3, 1) AS BLOB) || ?4
                            || CAST(substr(printf('%05d', value), 4, 1) AS BLOB) || ?4
                            || CAST(substr(printf('%05d', value), 5, 1) AS BLOB) || ?4
                         AS BLOB),
                    10001 - value,
                    NULL,
                    NULL,
                    'main',
                    value,
                    10001 - value
             FROM sequence",
            params![
                super::native_project_path_platform(),
                root_prefix,
                native_prefix,
                native_pad
            ],
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

#[asupersync::test]
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

#[asupersync::test]
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
        matches!(
            failure,
            tracedecay_domain::errors::TraceDecayError::Database { .. }
        ),
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
#[asupersync::test]
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

#[asupersync::test]
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

#[asupersync::test]
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
    let root = large_registry_root();
    assert_eq!(paths.first(), Some(&root.join("project-00001")));
    assert_eq!(paths.last(), Some(&root.join("project-10000")));
    assert_eq!(
        query.statements.get(),
        1,
        "listing cost must not grow with the number of registered projects"
    );
}

#[asupersync::test]
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

#[asupersync::test]
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

#[asupersync::test]
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

#[asupersync::test]
async fn dashboard_membership_returns_only_requested_registered_projects() {
    let harness = RegisteredGlobalDbHarness::open("dashboard-project-membership").await;
    let storage_root = harness
        .registered
        .db_path()
        .parent()
        .expect("registered database storage root");
    for project_id in ["project-a", "project-c"] {
        let root = storage_root.join(project_id);
        std::fs::create_dir_all(&root).expect("create membership project root");
        register(&harness.registered, project_id, &root).await;
    }

    let requested = vec![
        "project-a".to_owned(),
        "project-b".to_owned(),
        "project-c".to_owned(),
    ];
    assert_eq!(
        harness
            .registered
            .registered_code_project_ids(&requested)
            .await
            .expect("read project registration membership"),
        HashSet::from(["project-a".to_owned(), "project-c".to_owned()])
    );
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(plain_git_args(args))
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// One commit with fixed content and dates, so every repository made here
/// has the same root commit.
fn committed_repository(root: &Path) {
    std::fs::create_dir_all(root).expect("create repository");
    git(root, &["init", "-q"]);
    std::fs::write(root.join("lib.rs"), "pub fn a() {}\n").expect("write fixture file");
    git(root, &["add", "lib.rs"]);
    git(root, &["commit", "-q", "-m", "initial"]);
}

/// A committed repository and a full clone of it, as `(original, clone)`.
fn cloned_repository(harness: &RegisteredGlobalDbHarness, label: &str) -> (PathBuf, PathBuf) {
    let storage_root = harness
        .registered
        .db_path()
        .parent()
        .expect("registered database storage root");
    let original = storage_root.join(format!("{label}-original"));
    let clone = storage_root.join(format!("{label}-clone"));
    committed_repository(&original);
    git(
        storage_root,
        &[
            "clone",
            "-q",
            original.to_str().expect("utf-8 fixture path"),
            clone.to_str().expect("utf-8 fixture path"),
        ],
    );
    (original, clone)
}

async fn upsert_checkout(db: &RegisteredGlobalDb, project_id: &str, root: &Path) -> String {
    let common_dir = tracedecay_runtime_core::worktree::git_common_dir(root);
    let remote = tracedecay_runtime_core::git::git_remote_url(root);
    db.upsert_code_project(
        project_id,
        root,
        common_dir.as_deref(),
        remote.as_deref(),
        Some("main"),
    )
    .await
    .expect("register checkout")
    .project_id
}

fn root_commit_alias(root: &Path) -> String {
    let anchors = tracedecay_runtime_core::storage::repository_identity_anchors(root, None);
    assert_eq!(anchors.len(), 1, "fixture repository has one root commit");
    anchors[0].alias_key()
}

#[asupersync::test]
async fn upsert_adopts_the_project_a_root_commit_anchor_names() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-anchor-adoption").await;
    let (original, clone) = cloned_repository(&harness, "anchor-adoption");
    upsert_checkout(&harness.registered, "project-original", &original).await;

    assert_eq!(
        upsert_checkout(&harness.registered, "project-clone", &clone).await,
        "project-original",
        "a clone no path names adopts the project its root commit names"
    );
    let record = harness
        .registered
        .get_code_project("project-original")
        .await
        .expect("read adopted project")
        .expect("adopted project stays registered");
    assert_eq!(
        record.canonical_root,
        super::canonical_project_path(&original)
            .to_string_lossy()
            .into_owned(),
        "the first enrolled checkout stays the registered root"
    );
    assert!(
        harness
            .registered
            .get_code_project("project-clone")
            .await
            .expect("read clone id")
            .is_none(),
        "adoption mints no second project"
    );
}

#[asupersync::test]
async fn full_clone_adopts_a_shallow_first_project_and_enriches_its_history() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-shallow-first").await;
    let (original, full) = cloned_repository(&harness, "shallow-first");
    std::fs::write(original.join("lib.rs"), "pub fn next() {}\n").unwrap();
    git(&original, &["commit", "-q", "-am", "advance history"]);
    let origin = tracedecay_runtime_core::path_safety::plain_host_path(&original);
    let url = format!(
        "file:///{}/",
        tracedecay_domain::forward_slash_path(&origin).trim_start_matches('/')
    );
    git(&full, &["remote", "set-url", "origin", &url]);
    let shallow = original.with_file_name("shallow-first-shallow");
    git(
        original.parent().unwrap(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &url,
            shallow.to_str().unwrap(),
        ],
    );
    upsert_checkout(&harness.registered, "project-shallow", &shallow).await;
    assert!(
        !harness
            .registered
            .has_history_anchor("project-shallow")
            .await
            .unwrap()
    );

    assert_eq!(
        harness
            .registered
            .project_ids_by_identity(&full, None)
            .await
            .unwrap(),
        vec!["project-shallow"],
        "admission lookup resolves a full clone against the shallow-first remote"
    );
    assert_eq!(
        upsert_checkout(&harness.registered, "project-full", &full).await,
        "project-shallow"
    );
    assert!(
        harness
            .registered
            .has_history_anchor("project-shallow")
            .await
            .unwrap()
    );

    let unrelated = original.with_file_name("shallow-first-unrelated");
    committed_repository(&unrelated);
    std::fs::write(unrelated.join("lib.rs"), "pub fn unrelated() {}\n").unwrap();
    git(
        &unrelated,
        &["commit", "-q", "--amend", "-am", "unrelated root"],
    );
    git(&unrelated, &["remote", "add", "origin", &url]);
    assert!(
        harness
            .registered
            .project_ids_by_identity(&unrelated, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        upsert_checkout(&harness.registered, "project-unrelated", &unrelated).await,
        "project-unrelated",
        "once history is known a repointed template cannot adopt by remote alone"
    );
}

#[asupersync::test]
async fn clone_activity_keeps_the_verified_primary_root_listable() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-clone-recency").await;
    let (original, clone) = cloned_repository(&harness, "clone-recency");
    upsert_checkout(&harness.registered, "project-original", &original).await;
    let writer = harness.registered.writer_connection().unwrap();
    writer
        .execute_batch(
            "UPDATE code_projects SET last_seen_at = 100, primary_root_last_seen_at = 100;
         UPDATE project_aliases SET last_seen_at = 100;",
        )
        .await
        .unwrap();

    assert_eq!(
        upsert_checkout(&harness.registered, "project-clone", &clone).await,
        "project-original"
    );
    assert_eq!(
        harness
            .registered
            .try_list_code_project_paths(10)
            .await
            .unwrap(),
        vec![super::canonical_project_path(&original)],
        "new clone activity does not invalidate the primary root's own evidence"
    );
    upsert_checkout(&harness.registered, "project-original", &clone).await;
    assert_eq!(
        harness
            .registered
            .try_list_code_project_paths(10)
            .await
            .unwrap(),
        vec![super::canonical_project_path(&original)],
        "reopening an adopted clone preserves primary-root discovery"
    );
}

#[asupersync::test]
async fn anchors_do_not_keep_a_missing_project_alive_but_its_store_does() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-anchor-reap").await;
    let root = harness
        .registered
        .db_path()
        .parent()
        .unwrap()
        .join("reap-checkout");
    committed_repository(&root);
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/reap.git",
        ],
    );
    upsert_checkout(&harness.registered, "project-reap", &root).await;
    let anchor = root_commit_alias(&root);
    assert!(
        harness
            .registered
            .plan_registry_reap()
            .await
            .unwrap()
            .is_empty()
    );

    let store_root = tracedecay_runtime_core::storage::profile_sharded_data_root(
        harness.registered.db_path().parent().unwrap(),
        "project-reap",
    );
    std::fs::create_dir_all(&store_root).unwrap();
    let payload = store_root.join("fixture-data");
    std::fs::write(&payload, "durable fixture data").unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let retained = harness.registered.plan_registry_reap().await.unwrap();
    assert!(
        retained
            .retained
            .iter()
            .any(|entry| entry.entry.key == "project-reap")
    );
    harness
        .registered
        .apply_registry_reap(&retained)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&payload).unwrap(),
        "durable fixture data"
    );
    assert!(
        harness
            .registered
            .get_code_project("project-reap")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        harness
            .registered
            .project_id_by_alias_key(&anchor)
            .await
            .unwrap(),
        Some("project-reap".to_owned())
    );

    std::fs::remove_dir_all(&store_root).unwrap();
    let plan = harness.registered.plan_registry_reap().await.unwrap();
    assert!(plan.reapable.iter().any(
        |entry| entry.kind == super::ReapEntryKind::CodeProject && entry.key == "project-reap"
    ));
    harness.registered.apply_registry_reap(&plan).await.unwrap();
    assert!(
        harness
            .registered
            .get_code_project("project-reap")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        harness
            .registered
            .project_id_by_alias_key(&anchor)
            .await
            .unwrap()
            .is_none()
    );
}

// APFS rejects the non-UTF-8 directory name before registry admission.
#[cfg(target_os = "linux")]
#[asupersync::test]
async fn native_path_aliases_protect_roots_from_lossy_registry_reaping() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-native-reap").await;
    let root = harness
        .registered
        .db_path()
        .parent()
        .unwrap()
        .join(OsString::from_vec(b"native-root-\xff".to_vec()));
    std::fs::create_dir_all(&root).unwrap();
    register(&harness.registered, "project-native", &root).await;

    let plan = harness.registered.plan_registry_reap().await.unwrap();
    assert!(plan.reapable.is_empty());
    assert!(
        plan.retained
            .iter()
            .any(|entry| entry.entry.key == "project-native")
    );
    harness.registered.apply_registry_reap(&plan).await.unwrap();
    assert!(root.exists());
    assert_eq!(
        harness
            .registered
            .project_id_by_alias_key(&super::project_path_alias_key(&root))
            .await
            .unwrap(),
        Some("project-native".to_owned()),
        "lossy display paths must not allow a live native root to be unregistered"
    );
}

#[asupersync::test]
async fn an_anchor_keeps_the_project_that_first_recorded_it() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-anchor-stays").await;
    let (original, clone) = cloned_repository(&harness, "anchor-stays");
    upsert_checkout(&harness.registered, "project-original", &original).await;
    let anchor = root_commit_alias(&original);
    // A clone enrolled under its own id before anchors existed.
    let unrelated = original.with_file_name("anchor-stays-unrelated");
    std::fs::create_dir_all(&unrelated).expect("create unrelated root");
    register(&harness.registered, "project-clone", &unrelated).await;
    harness
        .registered
        .upsert_project_alias(&clone, "project-clone")
        .await
        .expect("alias the clone to its own project");

    assert_eq!(
        upsert_checkout(&harness.registered, "project-clone", &clone).await,
        "project-clone",
        "a clone a path already names keeps its own project"
    );
    assert_eq!(
        harness
            .registered
            .project_id_by_alias_key(&anchor)
            .await
            .expect("read anchor alias"),
        Some("project-original".to_owned()),
        "a later registration never re-points an anchor"
    );
}

#[asupersync::test]
async fn an_identical_history_without_a_remote_stays_its_own_project() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-anchor-no-remote").await;
    let storage_root = harness
        .registered
        .db_path()
        .parent()
        .expect("registered database storage root")
        .to_path_buf();
    let original = storage_root.join("no-remote-original");
    let lookalike = storage_root.join("no-remote-lookalike");
    committed_repository(&original);
    committed_repository(&lookalike);
    assert_eq!(root_commit_alias(&original), root_commit_alias(&lookalike));
    upsert_checkout(&harness.registered, "project-original", &original).await;

    assert_eq!(
        upsert_checkout(&harness.registered, "project-lookalike", &lookalike).await,
        "project-lookalike",
        "a repository that was not cloned does not adopt through its history"
    );
}

#[asupersync::test]
async fn a_clone_tracking_another_remote_does_not_adopt_through_history() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-anchor-fork").await;
    let (original, fork) = cloned_repository(&harness, "anchor-fork");
    upsert_checkout(&harness.registered, "project-original", &original).await;
    git(
        &fork,
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/fork.git",
        ],
    );

    assert_eq!(
        upsert_checkout(&harness.registered, "project-fork", &fork).await,
        "project-fork",
        "a shared root commit without remote agreement adopts nothing"
    );
}

#[asupersync::test]
async fn registering_a_named_checkout_again_does_not_walk_its_history() {
    let harness = RegisteredGlobalDbHarness::open("project-registry-anchor-walks").await;
    let (original, _clone) = cloned_repository(&harness, "anchor-walks");
    upsert_checkout(&harness.registered, "project-original", &original).await;
    let walks = tracedecay_runtime_core::storage::root_commit_walk_count_for_test(&original);
    assert_eq!(walks, 1, "first registration walks the checkout once");

    std::fs::write(original.join("lib.rs"), "pub fn b() {}\n").expect("edit fixture file");
    git(&original, &["commit", "-q", "-am", "move HEAD"]);
    upsert_checkout(&harness.registered, "project-original", &original).await;
    assert_eq!(
        tracedecay_runtime_core::storage::root_commit_walk_count_for_test(&original),
        walks,
        "a checkout whose project records its roots is not walked again"
    );
}

#[asupersync::test]
async fn stale_reap_plan_preserves_an_adopted_clone() {
    let harness = RegisteredGlobalDbHarness::open("registry-reap-adopted-clone").await;
    let (original, clone) = cloned_repository(&harness, "reap-adoption");
    upsert_checkout(&harness.registered, "project-original", &original).await;
    let anchor = root_commit_alias(&original);
    std::fs::remove_dir_all(&original).unwrap();
    let plan = harness.registered.plan_registry_reap().await.unwrap();
    assert!(plan.reapable.iter().any(|entry| {
        entry.kind == super::ReapEntryKind::CodeProject && entry.key == "project-original"
    }));

    assert_eq!(
        upsert_checkout(&harness.registered, "project-new", &clone).await,
        "project-original"
    );
    harness.registered.apply_registry_reap(&plan).await.unwrap();

    assert!(
        harness
            .registered
            .get_code_project("project-original")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        harness
            .registered
            .project_id_by_alias_key(&anchor)
            .await
            .unwrap(),
        Some("project-original".to_owned())
    );
    assert_eq!(
        harness
            .registered
            .project_id_by_alias_key(&super::project_path_alias_key(&clone))
            .await
            .unwrap(),
        Some("project-original".to_owned())
    );
}

#[asupersync::test]
async fn stale_reap_plan_preserves_a_new_store() {
    let harness = RegisteredGlobalDbHarness::open("registry-reap-new-store").await;
    let (root, _) = project_roots(&harness, "reap-store");
    register(&harness.registered, "project-store", &root).await;
    std::fs::remove_dir_all(&root).unwrap();
    let plan = harness.registered.plan_registry_reap().await.unwrap();
    assert!(plan.reapable.iter().any(|entry| {
        entry.kind == super::ReapEntryKind::CodeProject && entry.key == "project-store"
    }));

    let store = tracedecay_runtime_core::storage::profile_sharded_data_root(
        harness.registered.db_path().parent().unwrap(),
        "project-store",
    );
    std::fs::create_dir_all(&store).unwrap();
    let payload = store.join("fixture-data");
    std::fs::write(&payload, "durable fixture data").unwrap();
    harness.registered.apply_registry_reap(&plan).await.unwrap();

    assert!(
        harness
            .registered
            .get_code_project("project-store")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        std::fs::read_to_string(&payload).unwrap(),
        "durable fixture data"
    );
}

#[asupersync::test]
async fn stale_reap_plan_preserves_rebound_aliases_and_counts_deletions() {
    let harness = RegisteredGlobalDbHarness::open("registry-reap-rebound-alias").await;
    let (root, live) = project_roots(&harness, "reap-rebound");
    register(&harness.registered, "project-original", &root).await;
    std::fs::remove_dir_all(&root).unwrap();
    let mut plan = harness.registered.plan_registry_reap().await.unwrap();
    let authority = plan
        .reapable
        .iter()
        .find(|entry| {
            entry.kind == super::ReapEntryKind::CodeProject && entry.key == "project-original"
        })
        .unwrap()
        .clone();
    plan.reapable.push(authority);

    std::fs::create_dir_all(&live).unwrap();
    register(&harness.registered, "project-live", &live).await;
    harness
        .registered
        .upsert_project_alias(&root, "project-live")
        .await
        .unwrap();
    let removed = harness.registered.apply_registry_reap(&plan).await.unwrap();

    assert_eq!(
        harness
            .registered
            .project_id_by_alias_key(&super::project_path_alias_key(&root))
            .await
            .unwrap(),
        Some("project-live".to_owned())
    );
    assert_eq!(removed, 1, "only the original authority was deleted");
    assert_eq!(
        harness.registered.apply_registry_reap(&plan).await.unwrap(),
        0
    );
}

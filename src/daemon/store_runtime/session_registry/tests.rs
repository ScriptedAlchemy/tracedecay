use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tracedecay_store::CodeShardScopeV1;

use super::maintenance::RegisteredSchemaConvergenceStatus;
use super::{
    DaemonSessionRuntimeRegistryV1, DatabaseAccessMode, DatabaseAuthority,
    LocalProfileIdentityAuthorityV1, ProjectId, StoreShardIdV1, process_runtime_generation,
};
use crate::db::engine::{Executor, TestConnection};

async fn project_sessions_pending_convergence(
    project_name: &str,
) -> (
    tempfile::TempDir,
    LocalProfileIdentityAuthorityV1,
    ProjectId,
    PathBuf,
    PathBuf,
) {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new(project_name).expect("typed project identity");
    crate::storage::write_enrollment_marker(
        &project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("project enrollment");
    let sessions_path =
        crate::storage::profile_sharded_data_root(identity.profile_root(), project_id.as_str())
            .join(crate::storage::SESSIONS_DB_FILENAME);
    std::fs::create_dir_all(sessions_path.parent().expect("session database parent"))
        .expect("session database directory");
    let connection = TestConnection::open(&sessions_path);
    crate::global_db::ensure_registered_schema(&connection)
        .await
        .expect("seed complete registered schema");
    connection
        .execute("DELETE FROM authority_audit_checkpoints", ())
        .await
        .expect("remove durable convergence checkpoint");
    drop(connection);
    (temporary, identity, project_id, project_root, sessions_path)
}

async fn wait_for_schema_convergence(
    registry: &DaemonSessionRuntimeRegistryV1,
    shard_id: &StoreShardIdV1,
) -> RegisteredSchemaConvergenceStatus {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(status) = registry.registered_schema_convergence_status(shard_id)
                && !matches!(status, RegisteredSchemaConvergenceStatus::Pending)
            {
                return status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("registered schema convergence must reach a terminal state")
}

#[test]
fn fallback_runtime_generation_always_fits_sqlite_integer() {
    assert_eq!(
        process_runtime_generation("ffffffffffffffff0000000000000000"),
        Some(i64::MAX as u64)
    );
    assert_eq!(
        process_runtime_generation("00000000000000000000000000000000"),
        Some(1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_restart_fences_the_previous_session_runtime_binding() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    #[cfg(unix)]
    let endpoint =
        crate::daemon::transport::DaemonEndpoint::Unix(profile_root.join("session-runtime.sock"));
    #[cfg(not(unix))]
    let endpoint = crate::daemon::transport::default_loopback_endpoint();

    let first_authority =
        crate::daemon::authority::DaemonAuthority::acquire(&profile_root, &endpoint, "test")
            .expect("first daemon authority");
    let first_database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        first_authority.record().epoch,
        "first session runtime registry",
    )
    .expect("first daemon database scope");
    let identity = first_authority.profile_identity().clone();
    let first_registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("first session runtime registry");
    let stale = first_registry.profile_runtime.binding().clone();
    assert_eq!(
        stale.incarnation.get(),
        first_authority.record().epoch,
        "the durable daemon generation must own the store incarnation"
    );
    drop(first_registry);
    drop(first_database_scope);
    drop(first_authority);

    let second_authority =
        crate::daemon::authority::DaemonAuthority::acquire(&profile_root, &endpoint, "test")
            .expect("successor daemon authority");
    let _second_database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        second_authority.record().epoch,
        "successor session runtime registry",
    )
    .expect("successor daemon database scope");
    let second_registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("successor session runtime registry");
    let current = second_registry.profile_runtime.binding();

    assert_eq!(current.incarnation.get(), second_authority.record().epoch);
    assert!(current.incarnation > stale.incarnation);
    assert!(matches!(
        second_registry.registry.lookup(&stale),
        super::super::registry::StoreRuntimeLookup::WrongIncarnation {
            expected,
            actual,
        } if *expected == stale && actual.as_ref() == current
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn existing_profile_memory_is_migrated_before_exposure() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let memory_path = crate::memory::user::user_memory_db_path(identity.profile_root());
    let seed = TestConnection::open(&memory_path);
    crate::db::migrations::migrate_test_connection_to_version(&seed, 22)
        .await
        .expect("migrate profile memory fixture through production v22");
    drop(seed);

    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let database = registry
        .profile_memory()
        .await
        .expect("migrated profile memory");
    let mut rows = database
        .conn()
        .query(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'memory_v2_fact_relations'",
            (),
        )
        .await
        .expect("query migrated profile memory schema");
    let table_count: i64 = rows
        .next()
        .await
        .expect("read schema row")
        .expect("schema count row")
        .get(0)
        .expect("decode schema count");

    assert_eq!(table_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_sessions_mount_uses_the_durable_profile_identity_and_profile_pin() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let user_sessions_path = crate::sessions::user_sessions_db_path(identity.profile_root());

    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("session runtime registry");
    let registered = registry
        .profile_sessions()
        .await
        .expect("registered profile sessions");

    assert_eq!(
        &registered.binding().shard_id,
        &StoreShardIdV1::profile_sessions(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
        )
    );
    assert_eq!(registered.db_path(), user_sessions_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_sessions_mount_rejects_incompatible_schema_through_registered_runtime() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let seed_registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("schema seed runtime registry");
    let seeded_sessions = seed_registry
        .profile_sessions()
        .await
        .expect("seed registered profile sessions");
    seeded_sessions
        .writer_connection()
        .expect("schema corruption writer")
        .execute_batch(
            "DROP TABLE sessions;
                 CREATE TABLE sessions(provider TEXT NOT NULL);",
        )
        .await
        .expect("replace required session table with an incompatible shape");
    drop(seeded_sessions);
    drop(seed_registry);

    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let error = match registry.profile_sessions().await {
        Ok(_) => panic!("incompatible registered schema must fail closed"),
        Err(error) => error,
    };

    assert!(
        error.to_string().contains("no such column: project_key")
            && error.to_string().contains("initialize transcript schema"),
        "unexpected mount error: {error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_sessions_mount_uses_typed_enrollment_and_is_idempotent() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.session-runtime").expect("typed project identity");
    crate::storage::write_enrollment_marker(
        &project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("project enrollment");
    let sessions_path =
        crate::storage::profile_sharded_data_root(identity.profile_root(), project_id.as_str())
            .join(crate::storage::SESSIONS_DB_FILENAME);

    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("session runtime registry");
    let first = registry
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("registered project sessions");
    let second = registry
        .project_sessions(project_id.clone(), [project_root])
        .await
        .expect("idempotent project sessions");

    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(
        &first.binding().shard_id,
        &StoreShardIdV1::project_sessions(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            project_id,
        )
    );
    assert_eq!(first.db_path(), sessions_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_admission_returns_while_historical_convergence_is_blocked() {
    let (_temporary, identity, project_id, project_root, _sessions_path) =
        project_sessions_pending_convergence("project.schema-admission").await;
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    registry.enable_long_lived_session_maintenance_for_test();
    let convergence_gate = registry.block_registered_schema_convergence_for_test();

    let database = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        registry.project_sessions(project_id, [project_root]),
    )
    .await
    .expect("daemon admission must not wait for historical convergence")
    .expect("registered project sessions");
    convergence_gate.wait_until_blocked().await;

    assert_eq!(
        registry.registered_schema_convergence_status(&shard_id),
        Some(RegisteredSchemaConvergenceStatus::Pending)
    );
    let snapshot = database
        .read_snapshot()
        .await
        .expect("ordinary read snapshot while convergence is pending");
    let mut rows = snapshot
        .query("SELECT COUNT(*) FROM sessions", ())
        .await
        .expect("ordinary read while convergence is pending");
    assert_eq!(
        rows.next()
            .await
            .expect("read session count")
            .expect("session count row")
            .get::<i64>(0)
            .expect("decode session count"),
        0
    );
    convergence_gate.release();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_project_attaches_schedule_one_historical_convergence() {
    let (_temporary, identity, project_id, project_root, _sessions_path) =
        project_sessions_pending_convergence("project.schema-deduplication").await;
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    registry.enable_long_lived_session_maintenance_for_test();
    let convergence_gate = registry.block_registered_schema_convergence_for_test();

    let first = registry
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("first project session attach");
    convergence_gate.wait_until_blocked().await;
    let second = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("duplicate project session attach");

    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(
        registry.registered_schema_convergence_schedule_count_for_test(),
        1,
        "the retained registry must deduplicate convergence tasks"
    );
    convergence_gate.release();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_convergence_commits_the_durable_authority_checkpoint() {
    let (_temporary, identity, project_id, project_root, _sessions_path) =
        project_sessions_pending_convergence("project.schema-checkpoint").await;
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    registry.enable_long_lived_session_maintenance_for_test();
    let database = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("registered project sessions");

    assert_eq!(
        wait_for_schema_convergence(&registry, &shard_id).await,
        RegisteredSchemaConvergenceStatus::Complete
    );
    let snapshot = database
        .read_snapshot()
        .await
        .expect("checkpoint read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT bounded_passes_since_exhaustive
                 FROM authority_audit_checkpoints
                 WHERE audit_name = 'observation-authority'",
            (),
        )
        .await
        .expect("read durable authority checkpoint");
    assert_eq!(
        rows.next()
            .await
            .expect("read checkpoint")
            .expect("durable checkpoint row")
            .get::<i64>(0)
            .expect("decode checkpoint"),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_convergence_failure_remains_observable_as_degraded() {
    let (_temporary, identity, project_id, project_root, sessions_path) =
        project_sessions_pending_convergence("project.schema-degraded").await;
    rusqlite::Connection::open(&sessions_path)
        .expect("open corruption fixture")
        .execute_batch(
            "DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;
                 DROP TRIGGER IF EXISTS session_query_cursor_keys_retire_update_v1;
                 DROP TRIGGER IF EXISTS session_query_cursor_keys_rotate_insert_v1;
                 INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES
                    ('cursor-a', 1, X'01', 100, NULL),
                    ('cursor-b', 2, X'02', 200, NULL);",
        )
        .expect("seed corruption behind missing guards");
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    registry.enable_long_lived_session_maintenance_for_test();

    registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("minimum schema admission remains available");
    let status = wait_for_schema_convergence(&registry, &shard_id).await;

    assert!(
        matches!(
            status,
            RegisteredSchemaConvergenceStatus::Degraded { ref message }
                if message.contains("session cursor key rotation state is invalid")
        ),
        "unexpected convergence status: {status:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_project_sessions_reject_conflicting_enrollment_authority() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let first_project_root = root.join("project");
    let conflicting_project_root = root.join("conflicting-project");
    std::fs::create_dir_all(&first_project_root).expect("project root");
    std::fs::create_dir_all(&conflicting_project_root).expect("conflicting project root");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.session-runtime").expect("typed project identity");
    crate::storage::write_enrollment_marker(
        &first_project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("project enrollment");

    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    registry
        .project_sessions(project_id.clone(), [first_project_root])
        .await
        .expect("registered project sessions");
    let error = match registry
        .project_sessions(project_id, [conflicting_project_root])
        .await
    {
        Ok(_) => panic!("conflicting project enrollment authority must fail closed"),
        Err(error) => error,
    };

    assert!(
        error.to_string().contains("DuplicateProjectAuthority"),
        "unexpected authority error: {error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worktree_graph_mount_does_not_require_git() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("non-git project root");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let database_path = profile_root.join("stores/non-git-worktree.db");
    std::fs::create_dir_all(database_path.parent().expect("database parent"))
        .expect("database directory");
    let authority = DatabaseAuthority::acquire_test(&database_path, "non-git worktree graph mount")
        .expect("database authority");

    let database = registry
        .code_graph_worktree(
            &project_root,
            ProjectId::new("project.non-git-worktree").expect("project id"),
            database_path.clone(),
            authority,
            DatabaseAccessMode::ReadWrite,
        )
        .await
        .expect("non-git graph runtime");

    assert_eq!(database.database_path(), database_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_code_graph_open_rolls_back_new_authority_for_retry() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 13, "failed code graph retry")
            .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let database_root = profile_root.join("stores");
    std::fs::create_dir_all(&database_root).expect("database directory");
    let missing_path = database_root.join("missing-worktree.db");
    let replacement_path = database_root.join("replacement-worktree.db");
    let shard_id = StoreShardIdV1::code(
        registry.identity.brain_id().clone(),
        registry.identity.profile_id().clone(),
        ProjectId::new("project.failed-code-open").expect("project id"),
        tracedecay_store::RepositoryId::new("repository.failed-code-open").expect("repository id"),
        CodeShardScopeV1::Worktree {
            worktree_id: tracedecay_store::WorktreeId::new("worktree.failed-code-open")
                .expect("worktree id"),
        },
    );
    let missing_authority =
        DatabaseAuthority::acquire_test(&missing_path, "fail missing code graph open")
            .expect("missing database authority");

    let first_error = registry
        .code_graph_with_authority(
            shard_id.clone(),
            missing_path,
            Some(missing_authority),
            false,
        )
        .await
        .expect_err("missing code database must fail");
    assert!(
        first_error
            .to_string()
            .contains("resolved database does not exist"),
        "unexpected first open error: {first_error}"
    );

    rusqlite::Connection::open(&replacement_path)
        .expect("create replacement database")
        .execute_batch("CREATE TABLE replacement(value INTEGER);")
        .expect("seed replacement database");
    let replacement_authority =
        DatabaseAuthority::acquire_test(&replacement_path, "retry code graph open")
            .expect("replacement database authority");
    let runtime = registry
        .code_graph_with_authority(
            shard_id,
            replacement_path.clone(),
            Some(replacement_authority),
            false,
        )
        .await
        .expect("retry binds replacement after failed open");

    assert_eq!(runtime.locator().path(), replacement_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_retry_waits_for_failed_code_authority_rollback() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 14, "code graph open gate")
            .expect("daemon database scope");
    let database_root = profile_root.join("stores");
    std::fs::create_dir_all(&database_root).expect("database directory");
    let missing_path = database_root.join("concurrent-missing.db");
    let replacement_path = database_root.join("concurrent-replacement.db");
    rusqlite::Connection::open(&replacement_path)
        .expect("create replacement database")
        .execute_batch("CREATE TABLE replacement(value INTEGER);")
        .expect("seed replacement database");
    let registry = Arc::new(
        DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry"),
    );
    let shard = StoreShardIdV1::code(
        registry.identity.brain_id().clone(),
        registry.identity.profile_id().clone(),
        ProjectId::new("project.concurrent-code-open").expect("project id"),
        tracedecay_store::RepositoryId::new("repository.concurrent-code-open")
            .expect("repository id"),
        CodeShardScopeV1::Worktree {
            worktree_id: tracedecay_store::WorktreeId::new("worktree.concurrent-code-open")
                .expect("worktree id"),
        },
    );
    let blocker = registry.code_graph_open_guard(&shard).await;
    let first_registry = Arc::clone(&registry);
    let first_shard = shard.clone();
    let first = tokio::spawn(async move {
        let authority =
            DatabaseAuthority::acquire_test(&missing_path, "fail concurrent code graph open")
                .expect("missing database authority");
        first_registry
            .code_graph_with_authority(first_shard, missing_path, Some(authority), false)
            .await
    });
    wait_for_code_graph_gate_claims(&registry, &shard, 2).await;

    let second_registry = Arc::clone(&registry);
    let second_shard = shard.clone();
    let expected_path = replacement_path.clone();
    let second = tokio::spawn(async move {
        let authority =
            DatabaseAuthority::acquire_test(&replacement_path, "retry concurrent code graph open")
                .expect("replacement database authority");
        second_registry
            .code_graph_with_authority(second_shard, replacement_path, Some(authority), false)
            .await
    });
    wait_for_code_graph_gate_claims(&registry, &shard, 3).await;
    drop(blocker);

    let first_error = first
        .await
        .expect("failed-open task must join")
        .expect_err("missing code database must fail");
    assert!(
        first_error
            .to_string()
            .contains("resolved database does not exist"),
        "unexpected first open error: {first_error}"
    );
    let runtime = second
        .await
        .expect("retry task must join")
        .expect("retry must bind after failed authority rollback");
    assert_eq!(runtime.locator().path(), expected_path);

    let other_shard = StoreShardIdV1::code(
        registry.identity.brain_id().clone(),
        registry.identity.profile_id().clone(),
        ProjectId::new("project.other-code-open").expect("project id"),
        tracedecay_store::RepositoryId::new("repository.other-code-open").expect("repository id"),
        CodeShardScopeV1::Worktree {
            worktree_id: tracedecay_store::WorktreeId::new("worktree.other-code-open")
                .expect("worktree id"),
        },
    );
    let _other_guard = registry.code_graph_open_guard(&other_shard).await;
    assert!(
        !registry
            .code_graph_open_gates
            .lock()
            .await
            .contains_key(&shard),
        "completed shard gates must be pruned"
    );
}

async fn wait_for_code_graph_gate_claims(
    registry: &DaemonSessionRuntimeRegistryV1,
    shard: &StoreShardIdV1,
    expected: usize,
) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let strong_count = registry
                .code_graph_open_gates
                .lock()
                .await
                .get(shard)
                .map_or(0, Weak::strong_count);
            if strong_count >= expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("code graph open must claim the shard gate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_database_replacement_rebinds_after_runtime_retirement() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 12, "code replacement")
            .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let database_path = profile_root.join("stores/replaced-worktree.db");
    std::fs::create_dir_all(database_path.parent().expect("database parent"))
        .expect("database directory");
    let project_id = ProjectId::new("project.replaced-worktree").expect("project id");
    let authority =
        DatabaseAuthority::for_runtime(&database_path, "publish original code database")
            .expect("original database authority");
    let database = registry
        .code_graph_worktree(
            &project_root,
            project_id.clone(),
            database_path.clone(),
            authority,
            DatabaseAccessMode::ReadWrite,
        )
        .await
        .expect("original code database");
    database
        .checkpoint()
        .await
        .expect("checkpoint original database");
    drop(database);

    registry
        .close_code_graph_paths([database_path.clone()])
        .await
        .expect("retire original code runtime before replacement");
    let preserved_path = database_path.with_extension("db.preserved");
    std::fs::rename(&database_path, &preserved_path).expect("preserve original database");
    rusqlite::Connection::open(&database_path)
        .expect("create replacement database")
        .execute_batch("CREATE TABLE replacement(value INTEGER);")
        .expect("seed replacement database");

    let rebound = registry
        .code_graph_worktree(
            &project_root,
            project_id,
            database_path.clone(),
            DatabaseAuthority::for_runtime(&database_path, "publish replacement after retirement")
                .expect("rebound database authority"),
            DatabaseAccessMode::ReadWrite,
        )
        .await
        .expect("replacement code database must publish after retirement");
    assert_eq!(rebound.database_path(), database_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_branch_reuses_daemon_publication_without_write_authority() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    gix::init(&project_root).expect("initialize project repository");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 11, "branch publication")
            .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_id = ProjectId::new("project.branch-publication").expect("project id");
    let branch_root = profile_root.join("projects/project.branch-publication/branches");
    std::fs::create_dir_all(&branch_root).expect("branch database directory");
    let main_path = branch_root.join("main.db");
    let unpublished_path = branch_root.join("unpublished.db");
    rusqlite::Connection::open(&unpublished_path)
        .expect("seed unpublished branch database")
        .execute_batch("CREATE TABLE seed(value INTEGER);")
        .expect("seed unpublished branch schema");

    let main_authority = DatabaseAuthority::for_runtime(&main_path, "publish daemon-owned branch")
        .expect("daemon branch authority");
    assert_eq!(
        main_authority.role(),
        crate::db::DatabaseAuthorityRole::Daemon
    );
    let main = registry
        .code_graph_branch(
            &project_root,
            project_id.clone(),
            "main",
            main_path.clone(),
            main_authority,
            DatabaseAccessMode::ReadWrite,
        )
        .await
        .expect("daemon-owned branch publication");
    let publication_id = main.retained_runtime().publication().publication_id.clone();
    let unpublished_authority =
        DatabaseAuthority::for_runtime(&unpublished_path, "reserve unpublished branch")
            .expect("unpublished daemon branch authority");
    drop(database_scope);

    let read_only = registry
        .code_graph_branch_registered(
            &project_root,
            project_id.clone(),
            "main",
            main_path.clone(),
            DatabaseAccessMode::ReadOnly,
        )
        .await
        .expect("read-only facade over retained daemon publication");
    assert_eq!(read_only.database_path(), main_path);
    assert_eq!(
        read_only.retained_runtime().publication().publication_id,
        publication_id,
        "read-only publication must reuse the exact retained runtime"
    );
    let write_error = match read_only
        .begin_write_transaction("write through read-only branch facade")
        .await
    {
        Ok(_) => panic!("read-only branch facade unexpectedly admitted a write"),
        Err(error) => error,
    };
    assert!(
        write_error.to_string().contains("read-only"),
        "unexpected read-only denial: {write_error}"
    );

    let unpublished_error = match registry
        .code_graph_branch_registered(
            &project_root,
            project_id,
            "unpublished",
            unpublished_path,
            DatabaseAccessMode::ReadOnly,
        )
        .await
    {
        Ok(_) => panic!("unpublished branch inherited synthetic write authority"),
        Err(error) => error,
    };
    assert!(
        unpublished_error
            .to_string()
            .contains("managed-daemon or exclusive-maintenance authority"),
        "unexpected unpublished branch denial: {unpublished_error}"
    );
    drop(unpublished_authority);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_worktree_mount_never_recreates_a_deleted_database() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    gix::init(&project_root).expect("initialize project repository");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let database_path = profile_root.join("stores/worktree.db");
    std::fs::create_dir_all(database_path.parent().expect("database parent"))
        .expect("database directory");
    rusqlite::Connection::open(&database_path)
        .expect("seed worktree database")
        .execute_batch("CREATE TABLE seed(value INTEGER);")
        .expect("seed worktree schema");
    assert!(database_path.exists(), "lifecycle existence precheck");
    std::fs::remove_file(&database_path).expect("delete after lifecycle existence check");
    let database_authority =
        DatabaseAuthority::acquire_test(&database_path, "read-only deletion race")
            .expect("database authority");
    let result = registry
        .code_graph_worktree(
            &project_root,
            ProjectId::new("project.read-only-race").expect("project id"),
            database_path.clone(),
            database_authority,
            DatabaseAccessMode::ReadOnly,
        )
        .await;

    assert!(
        result.is_err(),
        "read-only mount must fail for a deleted DB"
    );
    assert!(
        !database_path.exists(),
        "read-only mount recreated the deleted worktree DB"
    );
}

use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};

use tokio::sync::Barrier;
use tracedecay_domain::{
    BrainNodeId, Confidence, FactCategoryV1, FactCurationActionV1, FactLineageEventKindV1,
    FactOwnerV1, FactRelationKindV1,
};
use tracedecay_rusqlite_runtime::remote::{
    RemoteSpoolKeyV1, RemoteSpoolKeyringV1, RemoteSqliteStorageErrorV1,
};

use super::maintenance::RegisteredSchemaConvergenceStatus;
use super::{
    DaemonSessionRuntimeRegistryV1, DatabaseAccessMode, DatabaseAuthority,
    LocalProfileIdentityAuthorityV1, ProjectId, StoreRuntimeRegistryFailure, StoreShardIdV1,
    TraceDecayError, process_runtime_generation, registry_open_error,
};
use crate::db::engine::{Executor, TestConnection};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationId, GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace,
    GraphProjectionId, GraphProjectionIdentity, GraphWatermark, SourceGeneration,
};
use tracedecay_store::{
    FactReadControl, FactWriteControl, ProjectMemoryFactHistoryQueryV1, ProjectMemoryFactIdV1,
    ProjectMemoryFactProjectionV1, RetainedGraphStoreLeaseV1,
};
use tracedecay_usecases::memory::{
    MemoryOperationContext, ProjectMemoryCurationMutationTarget, ProjectMemoryCurationOperation,
    ProjectMemoryFactAddRequest, ProjectMemoryFactAddRequestOutcome, memory_application_for_db,
};

struct TestRemoteKeyring(Arc<RemoteSpoolKeyV1>);

impl RemoteSpoolKeyringV1 for TestRemoteKeyring {
    fn active_key(&self) -> Result<Arc<RemoteSpoolKeyV1>, RemoteSqliteStorageErrorV1> {
        Ok(Arc::clone(&self.0))
    }

    fn key(
        &self,
        revision: u64,
    ) -> Result<Option<Arc<RemoteSpoolKeyV1>>, RemoteSqliteStorageErrorV1> {
        Ok((revision == self.0.revision()).then(|| Arc::clone(&self.0)))
    }
}

async fn project_sessions_pending_convergence(
    project_name: &str,
) -> (
    tempfile::TempDir,
    LocalProfileIdentityAuthorityV1,
    ProjectId,
    PathBuf,
    PathBuf,
    crate::db::DaemonDatabaseScope,
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
    // Production enters the daemon database scope before constructing the
    // session registry. Keep that authority alive for the whole fixture so
    // these daemon-maintenance tests never rely on ambient temp-path authority.
    let database_scope = crate::db::enter_daemon_database_scope(&profile_root, 1, project_name)
        .expect("daemon database scope");
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
    (
        temporary,
        identity,
        project_id,
        project_root,
        sessions_path,
        database_scope,
    )
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

fn accepting_memory_write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

async fn retention_fixture(
    capacity: usize,
) -> (
    tempfile::TempDir,
    DaemonSessionRuntimeRegistryV1,
    crate::db::DaemonDatabaseScope,
) {
    let temporary = tempfile::tempdir().expect("temporary runtime-retention root");
    let profile_root = temporary.path().join("profile");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "session-runtime-retention")
            .expect("daemon database scope");
    let registry =
        DaemonSessionRuntimeRegistryV1::open_with_retention_capacity_for_test(identity, capacity)
            .await
            .expect("bounded session runtime registry");
    (temporary, registry, database_scope)
}

async fn mount_retention_project(
    registry: &DaemonSessionRuntimeRegistryV1,
    root: &std::path::Path,
    label: &str,
) -> (
    ProjectId,
    Arc<crate::db::Database>,
    Arc<crate::global_db::RegisteredGlobalDb>,
) {
    let (project_id, project_root) = register_retention_project(root, label);
    let memory = registry
        .project_memory(project_id.clone(), [project_root.clone()])
        .await
        .expect("project memory runtime");
    let sessions = registry
        .project_sessions(project_id.clone(), [project_root])
        .await
        .expect("project session runtime");
    (project_id, memory, sessions)
}

fn register_retention_project(root: &std::path::Path, label: &str) -> (ProjectId, PathBuf) {
    let project_id =
        ProjectId::new(format!("runtime-retention-{label}")).expect("typed project identity");
    let project_root = root.join(label);
    std::fs::create_dir_all(&project_root).expect("project root");
    crate::storage::write_enrollment_marker(
        &project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("project enrollment");
    (project_id, project_root)
}

async fn add_schema_fact(
    database: &crate::db::Database,
    owner: FactOwnerV1,
    label: &str,
) -> ProjectMemoryCurationMutationTarget {
    let memory = memory_application_for_db(owner, database).expect("profile memory application");
    let preflight = memory
        .preflight_project_memory_fact_add(
            ProjectMemoryFactAddRequest {
                content: format!("canonical profile schema fixture {label}"),
                category: FactCategoryV1::UserPref,
                source_label: Some("profile-schema-verification".to_owned()),
                tags: vec![label.to_owned()],
                entities: Vec::new(),
                trust: Some(Confidence::new(0.9).expect("profile fact confidence")),
                metadata: serde_json::json!({"fixture": label}),
            },
            None,
        )
        .expect("preflight profile schema fact");
    let outcome = memory
        .add_preflighted_project_memory_fact(preflight, &accepting_memory_write_control())
        .await
        .expect("commit profile schema fact");
    let ProjectMemoryFactAddRequestOutcome::Applied(outcome) = outcome else {
        panic!("profile schema fixture must pass privacy admission");
    };
    let ProjectMemoryFactProjectionV1::Available(fact) = outcome.fact() else {
        panic!("profile schema fixture payload must remain available");
    };
    ProjectMemoryCurationMutationTarget::new(fact.fact_id().clone(), fact.last_event_id().clone())
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

#[test]
fn runtime_registry_reset_failure_remains_typed_for_project_open() {
    let error = registry_open_error(
        "open registered session runtime",
        StoreRuntimeRegistryFailure::ResetRequired {
            authority: "configuration".to_owned(),
            reason: "persisted shape is not final".to_owned(),
        },
    );

    assert!(matches!(
        error,
        TraceDecayError::ResetRequired { authority, reason }
            if authority == "configuration" && reason == "persisted shape is not final"
    ));
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
async fn existing_profile_memory_uses_final_schema_and_canonical_linked_lineage() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "existing profile memory schema")
            .expect("daemon database scope");
    let memory_path =
        tracedecay_runtime_core::memory::user::user_memory_db_path(identity.profile_root());
    let seed = TestConnection::open(&memory_path);
    crate::db::migrations::create_schema_connection(&seed)
        .await
        .expect("create the profile memory fixture at the production schema");
    drop(seed);

    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let database = registry
        .profile_memory()
        .await
        .expect("mounted profile memory");
    let source = add_schema_fact(&database, FactOwnerV1::Profile, "source").await;
    let target = add_schema_fact(&database, FactOwnerV1::Profile, "target").await;
    let evidence = add_schema_fact(&database, FactOwnerV1::Profile, "evidence").await;
    let owner = FactOwnerV1::Profile;
    let memory = memory_application_for_db(owner.clone(), &database)
        .expect("mounted profile memory application");
    memory
        .apply_project_memory_curation(
            vec![ProjectMemoryCurationOperation::LinkFacts {
                source: source.clone(),
                target,
                relation: FactRelationKindV1::Supports,
                evidence_facts: vec![evidence],
                confidence: Confidence::new(0.9).expect("profile relation confidence"),
                source_label: "profile-schema-verification".to_owned(),
                metadata: serde_json::json!({"fixture": "canonical-linked-lineage"}),
            }],
            Confidence::new(0.5).expect("profile curation threshold"),
            MemoryOperationContext::generated(
                &owner,
                "profile schema verification linked lineage",
                None,
            )
            .expect("profile curation operation identity"),
            None,
            &accepting_memory_write_control(),
        )
        .await
        .expect("commit canonical linked lineage");
    let history = memory
        .get_project_memory_history(
            ProjectMemoryFactHistoryQueryV1::new(
                ProjectMemoryFactIdV1::new(owner, source.fact_id().clone())
                    .expect("owner-bound profile source fact"),
                None,
                16,
            )
            .expect("bounded profile fact history query"),
            &FactReadControl::new(Arc::new(|| false)),
        )
        .await
        .expect("read canonical profile fact lineage");
    assert!(history.events().iter().any(|event| matches!(
        event.kind(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::Linked { relation },
            ..
        } if relation.kind() == FactRelationKindV1::Supports
    )));

    let mut rows = database
        .conn()
        .query(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'memory_v2_fact_relations'",
            (),
        )
        .await
        .expect("query mounted profile memory schema");
    let table_count: i64 = rows
        .next()
        .await
        .expect("read schema row")
        .expect("schema count row")
        .get(0)
        .expect("decode schema count");

    assert_eq!(
        table_count, 0,
        "mounted final memory must not recreate the retired relation projection"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_sessions_mount_uses_the_durable_profile_identity_and_profile_pin() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "profile sessions identity pin")
            .expect("daemon database scope");
    let user_sessions_path =
        tracedecay_sessions::runtime::user_sessions_db_path(identity.profile_root());

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
async fn remote_node_mount_uses_registered_identity_and_reuses_one_runtime() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    #[cfg(unix)]
    let endpoint =
        crate::daemon::transport::DaemonEndpoint::Unix(profile_root.join("remote-runtime.sock"));
    #[cfg(not(unix))]
    let endpoint = crate::daemon::transport::default_loopback_endpoint();
    let daemon_authority =
        crate::daemon::authority::DaemonAuthority::acquire(&profile_root, &endpoint, "test")
            .expect("daemon authority");
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        daemon_authority.record().epoch,
        "remote-node registry",
    )
    .expect("daemon database scope");
    let identity = daemon_authority.profile_identity().clone();
    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("session runtime registry");
    let node_id = BrainNodeId::new("node.remote.mount").expect("remote node identity");
    let secret = [5_u8; 32];
    let grant = crate::daemon::remote_protocol_tests::grant(
        identity.brain_id().clone(),
        node_id.clone(),
        &secret,
    );
    registry
        .provision_remote_node(
            grant.clone(),
            crate::daemon::remote_protocol_tests::admission(&grant),
        )
        .await
        .expect("authenticated first RemoteNode provisioning");
    let keyring = Arc::new(TestRemoteKeyring(Arc::new(
        RemoteSpoolKeyV1::from_secret_bytes(1, vec![7; 32]).expect("remote spool key"),
    )));

    let first = registry
        .remote_node_storage(node_id.clone(), keyring.clone())
        .await
        .expect("first remote-node mount");
    let first_recovery = registry
        .remote_recovery_authority(&node_id)
        .await
        .expect("mounted remote recovery authority");
    let second = registry
        .remote_node_storage(node_id.clone(), keyring)
        .await
        .expect("cached remote-node mount");
    let second_recovery = registry
        .remote_recovery_authority(&node_id)
        .await
        .expect("reused remote recovery authority");

    assert_eq!(first.binding(), second.binding());
    assert!(Arc::ptr_eq(&first_recovery, &second_recovery));
    assert_eq!(
        &first.binding().shard_id,
        &StoreShardIdV1::remote_node(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            node_id.clone(),
        )
    );
    drop(first_recovery);
    drop(second_recovery);
    drop(first);
    drop(second);
    drop(registry);

    let restarted = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("restarted session runtime registry");
    assert!(
        restarted
            .remote_recovery_authority(&node_id)
            .await
            .is_some(),
        "daemon startup must remount the persisted RemoteNode recovery authority"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_sessions_mount_rejects_incompatible_schema_through_registered_runtime() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "incompatible profile sessions schema",
    )
    .expect("daemon database scope");
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
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "typed project sessions enrollment",
    )
    .expect("daemon database scope");
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
    let (_temporary, identity, project_id, project_root, _sessions_path, _database_scope) =
        project_sessions_pending_convergence("project.schema-admission").await;
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
    let convergence_gate = registry.block_registered_schema_convergence_for_test();

    let admission = registry.project_sessions(project_id, [project_root]);
    tokio::pin!(admission);
    let convergence_blocked = convergence_gate.wait_until_blocked();
    tokio::pin!(convergence_blocked);
    let database = tokio::select! {
        result = &mut admission => {
            let database = result.expect("registered project sessions");
            convergence_blocked.await;
            database
        }
        () = &mut convergence_blocked => {
            tokio::time::timeout(std::time::Duration::from_secs(1), admission)
                .await
                .expect("daemon admission must not wait for blocked historical convergence")
                .expect("registered project sessions")
        }
    };

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
    let (_temporary, identity, project_id, project_root, _sessions_path, _database_scope) =
        project_sessions_pending_convergence("project.schema-deduplication").await;
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
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
    let (_temporary, identity, project_id, project_root, _sessions_path, _database_scope) =
        project_sessions_pending_convergence("project.schema-checkpoint").await;
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
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
    let (_temporary, identity, project_id, project_root, sessions_path, _database_scope) =
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
    let connection = TestConnection::open(&sessions_path);
    crate::global_db::schema_contract::ensure_authority_invariant_schema(&connection)
        .await
        .expect(
            "restore admission-critical guard triggers after seeding historical row corruption",
        );
    drop(connection);
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");

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
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "conflicting project sessions enrollment",
    )
    .expect("daemon database scope");
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
    let project_id = ProjectId::new("project.non-git-worktree").expect("project id");
    crate::storage::write_enrollment_marker(
        &project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("project enrollment");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 13, "non-git worktree graph")
            .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    registry
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("register non-git project authority");
    let project_store_root = profile_root.join("projects/project.non-git-worktree");
    let database_path = project_store_root.join(crate::config::db_filename(&project_store_root));
    std::fs::create_dir_all(database_path.parent().expect("database parent"))
        .expect("database directory");
    let authority = DatabaseAuthority::for_runtime(&database_path, "non-git project graph mount")
        .expect("database authority");

    let database = registry
        .project_graph(
            &project_root,
            project_id,
            database_path.clone(),
            authority,
            DatabaseAccessMode::ReadWrite,
        )
        .await
        .expect("non-git graph runtime");

    assert_eq!(database.database_path(), database_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_graph_runtime_publishes_recovers_and_fails_closed() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary.path().canonicalize().expect("canonical root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.generic-graph").expect("project id");
    crate::storage::write_enrollment_marker(
        &project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("project enrollment");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 19, "generic graph runtime")
            .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_database = registry
        .project_memory(project_id.clone(), [project_root])
        .await
        .expect("project database");
    let runtime = project_database
        .memory_graph_runtime()
        .expect("project graph runtime");
    let projection = GraphProjectionIdentity::new(
        GraphNamespace::new("journey:generic-test").expect("namespace"),
        GraphProjectionId::new("projection.generic-test").expect("projection"),
    );
    let manifest = GraphGenerationManifest::new(
        projection.clone(),
        GraphGenerationId::new("generation.generic-test.1").expect("generation"),
        SourceGeneration::new("source.generic-test.1").expect("source"),
        GraphWatermark::new("watermark.generic-test.1").expect("watermark"),
        vec![],
        vec![],
        vec![],
    )
    .expect("manifest");
    let idempotency = GraphIdempotencyKey::new("idempotency.generic-test.1").expect("idempotency");

    let published = runtime
        .publish_verified_manifest(
            &manifest,
            idempotency.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("publish inline manifest");
    assert_eq!(published.generation(), &manifest.generation);
    let replayed = runtime
        .publish_verified_manifest(&manifest, idempotency, Arc::new(AtomicBool::new(false)))
        .expect("recover exact publication");
    assert_eq!(replayed.generation(), &manifest.generation);
    let recovered = runtime
        .verified_snapshot(&projection, FactReadControl::new(Arc::new(|| false)))
        .expect("recover verified head")
        .expect("published verified head");
    assert_eq!(recovered.generation(), &manifest.generation);

    let successor = GraphGenerationManifest::new(
        projection.clone(),
        GraphGenerationId::new("generation.generic-test.2").expect("successor generation"),
        SourceGeneration::new("source.generic-test.2").expect("successor source"),
        GraphWatermark::new("watermark.generic-test.2").expect("successor watermark"),
        vec![],
        vec![],
        vec![],
    )
    .expect("successor manifest");
    runtime
        .publish_verified_manifest(
            &successor,
            GraphIdempotencyKey::new("idempotency.generic-test.2").expect("successor idempotency"),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("publish successor");
    assert!(matches!(
        runtime.publish_verified_manifest(
            &manifest,
            GraphIdempotencyKey::new("idempotency.generic-test.1").expect("stale idempotency"),
            Arc::new(AtomicBool::new(false)),
        ),
        Err(GraphDbError::Conflict)
    ));

    let cancelled = FactReadControl::new(Arc::new(|| true));
    assert!(matches!(
        runtime.verified_snapshot(&projection, cancelled),
        Err(GraphDbError::Cancelled)
    ));
    let missing = GraphProjectionIdentity::new(
        projection.namespace.clone(),
        GraphProjectionId::new("projection.generic-test.missing").expect("missing projection"),
    );
    // Never published: the typed empty start, not an unavailability error.
    assert!(matches!(
        runtime.verified_snapshot(&missing, FactReadControl::new(Arc::new(|| false))),
        Ok(None)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_worktree_generations_share_the_project_graph_runtime() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let primary_root = root.join("primary");
    let linked_root = root.join("linked");
    std::fs::create_dir_all(&primary_root).expect("primary project root");
    std::fs::create_dir_all(&linked_root).expect("linked project root");
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.shared-code-graph").expect("project id");
    for project_root in [&primary_root, &linked_root] {
        crate::storage::write_enrollment_marker(
            project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .expect("project enrollment");
    }
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 17, "shared code graph")
            .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_database = registry
        .project_memory(
            project_id.clone(),
            [primary_root.clone(), linked_root.clone()],
        )
        .await
        .expect("mount project authority");
    let repository_id = tracedecay_domain::RepositoryId::new("repo:shared").expect("repository id");
    let replay_binding = || crate::daemon::code_index_scheduler::CodeGraphReplayBindingV1 {
        generations_root: project_database
            .database_path()
            .with_extension("generations"),
        sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest::try_from(format!(
            "sha256:{}",
            "9".repeat(64)
        ))
        .expect("synthetic sealed digest"),
    };
    let primary = registry
        .retain_code_graph_runtime(
            project_id.clone(),
            repository_id.clone(),
            tracedecay_domain::WorktreeId::new("worktree:primary").expect("primary worktree"),
            None,
            tracedecay_domain::CodeGenerationId::new("generation:primary")
                .expect("primary generation"),
            Arc::clone(&project_database),
            replay_binding(),
        )
        .await
        .expect("primary graph runtime");
    let linked = registry
        .retain_code_graph_runtime(
            project_id,
            repository_id,
            tracedecay_domain::WorktreeId::new("worktree:linked").expect("linked worktree"),
            None,
            tracedecay_domain::CodeGenerationId::new("generation:linked")
                .expect("linked generation"),
            Arc::clone(&project_database),
            replay_binding(),
        )
        .await
        .expect("linked graph runtime");

    let primary_authority = primary.authority();
    let linked_authority = linked.authority();
    assert_eq!(primary_authority.binding(), linked_authority.binding());
    assert_ne!(primary_authority.namespace(), linked_authority.namespace());
    assert_eq!(
        primary_authority.canonical_path(),
        project_database.database_path().with_extension("grafeo")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_project_graph_reuses_daemon_publication_without_write_authority() {
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
        crate::db::enter_daemon_database_scope(&profile_root, 11, "project graph publication")
            .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_id = ProjectId::new("project.graph-publication").expect("project id");
    crate::storage::write_enrollment_marker(
        &project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("project enrollment");
    registry
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("register project authority");
    let project_store_root = profile_root.join("projects/project.graph-publication");
    std::fs::create_dir_all(&project_store_root).expect("project store directory");
    let main_path = project_store_root.join(crate::config::db_filename(&project_store_root));
    let unpublished_path = project_store_root.join("unpublished.db");
    rusqlite::Connection::open(&unpublished_path)
        .expect("seed unpublished branch database")
        .execute_batch("CREATE TABLE seed(value INTEGER);")
        .expect("seed unpublished branch schema");

    let main_authority =
        DatabaseAuthority::for_runtime(&main_path, "publish daemon-owned project graph")
            .expect("daemon project graph authority");
    assert_eq!(
        main_authority.role(),
        crate::db::DatabaseAuthorityRole::Daemon
    );
    let main = registry
        .project_graph(
            &project_root,
            project_id.clone(),
            main_path.clone(),
            main_authority,
            DatabaseAccessMode::ReadWrite,
        )
        .await
        .expect("daemon-owned project graph publication");
    let publication_id = main.retained_runtime().publication().publication_id.clone();
    let unpublished_authority =
        DatabaseAuthority::for_runtime(&unpublished_path, "reserve unpublished project store")
            .expect("unpublished daemon project-store authority");
    drop(database_scope);

    let read_only = registry
        .project_graph_registered(
            project_id.clone(),
            main_path.clone(),
            DatabaseAccessMode::ReadOnly,
        )
        .await
        .expect("read-only facade over retained project graph publication");
    assert_eq!(read_only.database_path(), main_path);
    assert_eq!(
        read_only.retained_runtime().publication().publication_id,
        publication_id,
        "read-only publication must reuse the exact retained runtime"
    );
    let write_error = match read_only
        .begin_write_transaction("write through read-only project graph facade")
        .await
    {
        Ok(_) => panic!("read-only project graph facade unexpectedly admitted a write"),
        Err(error) => error,
    };
    assert!(
        write_error.to_string().contains("read-only"),
        "unexpected read-only denial: {write_error}"
    );

    let unpublished_error = match registry
        .project_graph_registered(project_id, unpublished_path, DatabaseAccessMode::ReadOnly)
        .await
    {
        Ok(_) => panic!("unpublished project store inherited synthetic write authority"),
        Err(error) => error,
    };
    assert!(
        unpublished_error
            .to_string()
            .contains("differs from retained canonical locator"),
        "unexpected unpublished project store denial: {unpublished_error}"
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
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "read-only deleted worktree mount",
    )
    .expect("daemon database scope");
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
        .project_graph(
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

#[tokio::test]
async fn project_runtime_capacity_retires_idle_pairs_and_reopens_the_exact_project() {
    let (temporary, registry, _database_scope) = retention_fixture(2).await;
    let (first_id, first_memory, first_sessions) =
        mount_retention_project(&registry, temporary.path(), "first").await;
    drop((first_memory, first_sessions));
    let (_second_id, second_memory, second_sessions) =
        mount_retention_project(&registry, temporary.path(), "second").await;
    drop((second_memory, second_sessions));

    let (_third_id, third_memory, third_sessions) =
        mount_retention_project(&registry, temporary.path(), "third").await;
    drop((third_memory, third_sessions));

    let settled = registry
        .session_runtime_retention_telemetry()
        .await
        .expect("retention telemetry");
    assert_eq!(settled.project_memory_runtimes, 2);
    assert_eq!(settled.project_session_runtimes, 2);
    assert_eq!(settled.retired_project_memory_runtimes, 1);
    assert_eq!(settled.retired_project_session_runtimes, 1);

    let (reopened_id, reopened_memory, reopened_sessions) =
        mount_retention_project(&registry, temporary.path(), "first").await;
    assert_eq!(reopened_id, first_id);
    assert!(reopened_memory.is_writable());
    assert_eq!(
        reopened_sessions.binding().shard_id.scope.project_id(),
        Some(&first_id)
    );
}

#[tokio::test]
async fn project_runtime_capacity_never_evicts_a_live_project_pair() {
    let (temporary, registry, _database_scope) = retention_fixture(1).await;
    let (first_id, first_memory, first_sessions) =
        mount_retention_project(&registry, temporary.path(), "live-first").await;
    let owner = FactOwnerV1::Project {
        project_id: first_id.clone(),
    };
    let fact = add_schema_fact(&first_memory, owner.clone(), "retention-refusal").await;
    let runtime_client = first_memory.retained_runtime().clone();
    let graph_lease = first_memory
        .memory_graph_runtime()
        .expect("project memory graph lease");
    let reconciliation_lease = first_memory
        .memory_graph_reconciliation_task_owner()
        .expect("project memory reconciliation lease");

    let blocked = registry
        .project_memory(
            ProjectId::new("runtime-retention-blocked").expect("typed project identity"),
            [temporary.path().join("blocked")],
        )
        .await
        .expect_err("a live project pair must retain its exact runtime authority");
    assert!(
        blocked
            .to_string()
            .contains("project runtime retention capacity is exhausted"),
        "unexpected bounded-retirement refusal: {blocked}"
    );
    assert!(first_memory.is_writable());
    assert_eq!(
        first_memory.retained_runtime().binding(),
        runtime_client.binding()
    );
    assert_eq!(
        first_sessions.binding().shard_id.scope.project_id(),
        Some(&first_id)
    );
    let graph_projection = GraphProjectionIdentity::new(
        GraphNamespace::new("journey:retention-refusal").expect("graph namespace"),
        GraphProjectionId::new("projection.retention-refusal").expect("graph projection"),
    );
    assert!(
        graph_lease
            .verified_snapshot(&graph_projection, FactReadControl::new(Arc::new(|| false)))
            .expect("retained graph lease remains readable after refusal")
            .is_none(),
        "the retained graph lease must report a typed empty graph, not cancellation or closure"
    );
    registry
        .retain_memory_graph_reconciliation_task(
            &first_memory.retained_runtime().binding().shard_id,
            first_memory.as_ref(),
        )
        .expect("retained reconciliation lease remains admitted after refusal");
    let retained_reconciliation_lease = first_memory
        .memory_graph_reconciliation_task_owner()
        .expect("retained project memory reconciliation lease");
    assert!(
        reconciliation_lease.same_coordinator(&retained_reconciliation_lease),
        "capacity refusal must preserve the exact reconciliation coordinator"
    );
    let memory = memory_application_for_db(owner.clone(), first_memory.as_ref())
        .expect("live project memory application");
    let history = memory
        .get_project_memory_history(
            ProjectMemoryFactHistoryQueryV1::new(
                ProjectMemoryFactIdV1::new(owner, fact.fact_id().clone())
                    .expect("owner-bound retained fact"),
                None,
                16,
            )
            .expect("bounded retained fact history"),
            &FactReadControl::new(Arc::new(|| false)),
        )
        .await
        .expect("durable fact remains readable after refusal");
    assert!(
        !history.events().is_empty(),
        "capacity refusal must preserve the durable retained fact"
    );
    let telemetry = registry
        .session_runtime_retention_telemetry()
        .await
        .expect("retention telemetry");
    assert_eq!(telemetry.project_memory_runtimes, 1);
    assert_eq!(telemetry.project_session_runtimes, 1);
    assert_eq!(telemetry.retirement_refusals, 1);
}

#[tokio::test]
async fn runtime_close_refusal_preserves_the_exact_memory_graph_and_reconciliation_leases() {
    let (temporary, registry, _database_scope) = retention_fixture(1).await;
    let (project_id, project_root) = register_retention_project(temporary.path(), "rollback");
    let database = registry
        .project_memory(project_id.clone(), [project_root.clone()])
        .await
        .expect("project memory runtime");
    let owner = FactOwnerV1::Project {
        project_id: project_id.clone(),
    };
    let fact = add_schema_fact(&database, owner.clone(), "retirement-rollback").await;
    let runtime_client = database.retained_runtime().clone();
    let graph_lease = database
        .memory_graph_runtime()
        .expect("project memory graph lease");
    let reconciliation_lease = database
        .memory_graph_reconciliation_task_owner()
        .expect("project memory reconciliation lease");
    drop(database);

    let refusal = registry
        .retire_project_memory_graph(&project_id)
        .await
        .expect_err("a retained runtime client must refuse exact memory retirement");
    assert!(
        refusal.to_string().contains("close project memory runtime"),
        "unexpected runtime-close refusal: {refusal}"
    );
    let graph_projection = GraphProjectionIdentity::new(
        GraphNamespace::new("journey:retention-rollback").expect("graph namespace"),
        GraphProjectionId::new("projection.retention-rollback").expect("graph projection"),
    );
    assert!(
        graph_lease
            .verified_snapshot(&graph_projection, FactReadControl::new(Arc::new(|| false)))
            .expect("exact graph lease remains readable after runtime-close refusal")
            .is_none(),
        "the exact graph lease must remain an available empty graph after refusal"
    );

    let reopened = registry
        .project_memory(project_id.clone(), [project_root])
        .await
        .expect("exact project memory route remains available after refusal");
    assert_eq!(
        reopened.retained_runtime().binding(),
        runtime_client.binding()
    );
    let reopened_reconciliation_lease = reopened
        .memory_graph_reconciliation_task_owner()
        .expect("restored project memory reconciliation lease");
    assert!(
        reconciliation_lease.same_coordinator(&reopened_reconciliation_lease),
        "runtime-close refusal must not replace the exact reconciliation coordinator"
    );
    let memory = memory_application_for_db(owner.clone(), reopened.as_ref())
        .expect("reopened project memory application");
    let history = memory
        .get_project_memory_history(
            ProjectMemoryFactHistoryQueryV1::new(
                ProjectMemoryFactIdV1::new(owner, fact.fact_id().clone())
                    .expect("owner-bound rollback fact"),
                None,
                16,
            )
            .expect("bounded rollback fact history"),
            &FactReadControl::new(Arc::new(|| false)),
        )
        .await
        .expect("durable fact remains readable after rollback refusal");
    assert!(
        !history.events().is_empty(),
        "runtime-close refusal must preserve the durable fact"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_distinct_project_memory_opens_never_overflow_retained_capacity() {
    let (temporary, registry, _database_scope) = retention_fixture(1).await;
    let registry = Arc::new(registry);
    let projects = ["memory-race-a", "memory-race-b", "memory-race-c"]
        .into_iter()
        .map(|label| register_retention_project(temporary.path(), label))
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(projects.len()));
    let mut opens = tokio::task::JoinSet::new();
    for (project_id, project_root) in projects {
        let registry = Arc::clone(&registry);
        let barrier = Arc::clone(&barrier);
        opens.spawn(async move {
            barrier.wait().await;
            registry.project_memory(project_id, [project_root]).await
        });
    }

    let mut admitted = Vec::new();
    while let Some(open) = opens.join_next().await {
        if let Ok(database) = open.expect("project memory open task") {
            admitted.push(database);
        }
    }

    let telemetry = registry
        .session_runtime_retention_telemetry()
        .await
        .expect("retention telemetry after concurrent project-memory opens");
    assert!(
        admitted.len() <= 1,
        "simultaneous live project-memory opens exceeded the capacity: {}",
        admitted.len()
    );
    assert!(
        telemetry.project_memory_runtimes <= telemetry.project_runtime_capacity,
        "project-memory registry cardinality {} exceeded capacity {}",
        telemetry.project_memory_runtimes,
        telemetry.project_runtime_capacity
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_distinct_project_session_opens_never_overflow_retained_capacity() {
    let (temporary, registry, _database_scope) = retention_fixture(1).await;
    let registry = Arc::new(registry);
    let projects = ["sessions-race-a", "sessions-race-b", "sessions-race-c"]
        .into_iter()
        .map(|label| register_retention_project(temporary.path(), label))
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(projects.len()));
    let mut opens = tokio::task::JoinSet::new();
    for (project_id, project_root) in projects {
        let registry = Arc::clone(&registry);
        let barrier = Arc::clone(&barrier);
        opens.spawn(async move {
            barrier.wait().await;
            registry.project_sessions(project_id, [project_root]).await
        });
    }

    let mut admitted = Vec::new();
    while let Some(open) = opens.join_next().await {
        if let Ok(database) = open.expect("project session open task") {
            admitted.push(database);
        }
    }

    let telemetry = registry
        .session_runtime_retention_telemetry()
        .await
        .expect("retention telemetry after concurrent project-session opens");
    assert!(
        admitted.len() <= 1,
        "simultaneous live project-session opens exceeded the capacity: {}",
        admitted.len()
    );
    assert!(
        telemetry.project_session_runtimes <= telemetry.project_runtime_capacity,
        "project-session registry cardinality {} exceeded capacity {}",
        telemetry.project_session_runtimes,
        telemetry.project_runtime_capacity
    );
}

#[tokio::test]
async fn runtime_shutdown_joins_memory_reconciliation_and_reopens_cleanly() {
    let (temporary, registry, _database_scope) = retention_fixture(1).await;
    let (project_id, memory, sessions) =
        mount_retention_project(&registry, temporary.path(), "shutdown").await;
    drop((memory, sessions));

    registry
        .shutdown_retained_runtimes()
        .await
        .expect("cancellation-safe retained-runtime shutdown");
    let shutdown = registry
        .session_runtime_retention_telemetry()
        .await
        .expect("shutdown telemetry");
    assert_eq!(shutdown.project_memory_runtimes, 0);
    assert_eq!(shutdown.project_session_runtimes, 0);
    assert_eq!(shutdown.retained_memory_graph_reconciliation_tasks, 0);

    drop(registry);
    let identity =
        crate::daemon::profile_identity::load_or_create(&temporary.path().join("profile"))
            .expect("reopen durable profile identity");
    let registry =
        DaemonSessionRuntimeRegistryV1::open_with_retention_capacity_for_test(identity, 1)
            .await
            .expect("fresh runtime registry after cancellation-safe shutdown");
    let (reopened_id, reopened_memory, reopened_sessions) =
        mount_retention_project(&registry, temporary.path(), "shutdown").await;
    assert_eq!(reopened_id, project_id);
    assert!(reopened_memory.is_writable());
    assert_eq!(
        reopened_sessions.binding().shard_id.scope.project_id(),
        Some(&project_id)
    );
}

//! Composition-root integration tests for the store-runtime crate.
//!
//! These tests reach `remote_protocol_tests` and `crate::config`, which compose
//! root daemon fixtures and therefore cannot live in
//! `tracedecay-store-runtime` itself.

use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};

use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{
    BrainNodeId, Confidence, FactCategoryV1, FactCurationActionV1, FactLineageEventKindV1,
    FactOwnerV1, FactRelationKindV1,
};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationId, GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace,
    GraphProjectionId, GraphProjectionIdentity, GraphWatermark, SourceGeneration,
};
use tracedecay_runtime_core::db::engine::{QueryExecutor, TestConnection};
use tracedecay_runtime_core::db::{DatabaseAccessMode, DatabaseAuthority};
use tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRegistryFailure;
use tracedecay_rusqlite_runtime::remote::{
    RemoteSpoolKeyV1, RemoteSpoolKeyringV1, RemoteSqliteStorageErrorV1,
};
use tracedecay_session_memory::memory::{
    MemoryOperationContext, ProjectMemoryCurationMutationTarget, ProjectMemoryCurationOperation,
    ProjectMemoryFactAddRequest, ProjectMemoryFactAddRequestOutcome, memory_application_for_db,
};
use tracedecay_store::{
    FactReadControl, FactWriteControl, ProjectMemoryFactHistoryQueryV1, ProjectMemoryFactIdV1,
    ProjectMemoryFactProjectionV1, RetainedGraphStoreLeaseV1,
};
use tracedecay_store::{ProjectId, StoreShardIdV1};
use tracedecay_store_runtime::{
    DaemonSessionRuntimeRegistryV1, RegisteredSchemaConvergenceStatus, process_runtime_generation,
    register_registered_schema_installer, registry_open_error,
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
    tracedecay_runtime_core::db::DaemonDatabaseScope,
) {
    project_sessions_convergence_fixture(project_name, true).await
}

async fn project_sessions_current_convergence(
    project_name: &str,
) -> (
    tempfile::TempDir,
    LocalProfileIdentityAuthorityV1,
    ProjectId,
    PathBuf,
    PathBuf,
    tracedecay_runtime_core::db::DaemonDatabaseScope,
) {
    project_sessions_convergence_fixture(project_name, false).await
}

async fn project_sessions_convergence_fixture(
    project_name: &str,
    remove_checkpoint: bool,
) -> (
    tempfile::TempDir,
    LocalProfileIdentityAuthorityV1,
    ProjectId,
    PathBuf,
    PathBuf,
    tracedecay_runtime_core::db::DaemonDatabaseScope,
) {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    // Production enters the daemon database scope before constructing the
    // session registry. Keep that authority alive for the whole fixture so
    // these daemon-maintenance tests never rely on ambient temp-path authority.
    let database_scope =
        tracedecay_runtime_core::db::enter_daemon_database_scope(&profile_root, 1, project_name)
            .expect("daemon database scope");
    let project_id = ProjectId::new(project_name).expect("typed project identity");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let sessions_path = tracedecay_runtime_core::storage::profile_sharded_data_root(
        identity.profile_root(),
        project_id.as_str(),
    )
    .join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);
    std::fs::create_dir_all(sessions_path.parent().expect("session database parent"))
        .expect("session database directory");
    register_registered_schema_installer();
    crate::register_runtime_ports().expect("runtime port registration");
    let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
        &sessions_path,
        "seed project sessions registered schema fixture",
    )
    .expect("project sessions fixture database authority");
    let (database, _) = tracedecay_runtime_core::db::Database::publish_registered_test_runtime_for_profile_identity(
        &sessions_path,
        &authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
        tracedecay_runtime_core::db::TestRuntimeProfileIdentityV1::new(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
        ),
        tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProjectSessions {
            project_id: project_id.clone(),
        },
    )
    .await
    .expect("seed complete registered schema");
    if remove_checkpoint {
        database
            .execute_write_batch(
                "remove durable convergence checkpoint",
                "DELETE FROM authority_audit_checkpoints",
            )
            .await
            .expect("remove durable convergence checkpoint");
    }
    drop(database);
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
                && !matches!(
                    status,
                    RegisteredSchemaConvergenceStatus::Pending
                        | RegisteredSchemaConvergenceStatus::Running
                )
            {
                return status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("registered schema convergence must reach a terminal state")
}

const LCM_STATUS_PERFORMANCE_INDEX_NAMES: [&str; 4] = [
    "idx_lcm_raw_legacy_truncated",
    "idx_lcm_raw_lossy_ingest",
    "idx_lcm_summary_nodes_depth_tokens",
    "idx_lcm_external_payloads_owner_bytes",
];
const SUPERSEDED_LCM_PAYLOAD_OWNER_INDEX: &str = "idx_lcm_external_payloads_owner";

async fn installed_lcm_status_index_names(
    connection: &(impl QueryExecutor + ?Sized),
) -> Vec<String> {
    let mut rows = connection
        .query(
            "SELECT name
             FROM sqlite_master
             WHERE type = 'index'
               AND name IN (
                   'idx_lcm_raw_legacy_truncated',
                   'idx_lcm_raw_lossy_ingest',
                   'idx_lcm_summary_nodes_depth_tokens',
                   'idx_lcm_external_payloads_owner_bytes',
                   'idx_lcm_external_payloads_owner'
               )
             ORDER BY name",
            (),
        )
        .await
        .expect("read LCM status index names");
    let mut names = Vec::new();
    while let Some(row) = rows.next().await.expect("read LCM status index name") {
        names.push(row.get::<String>(0).expect("decode LCM status index name"));
    }
    names
}

async fn deferred_lcm_fixture_row_count(connection: &(impl QueryExecutor + ?Sized)) -> i64 {
    let mut rows = connection
        .query(
            "SELECT COUNT(*)
             FROM sessions AS session
             JOIN lcm_raw_messages AS message
               ON message.provider = session.provider
              AND message.session_id = session.session_id
             WHERE session.session_id = 'deferred-index-session'
               AND message.message_id = 'deferred-index-message'",
            (),
        )
        .await
        .expect("read seeded session and LCM row");
    rows.next()
        .await
        .expect("read seeded session and LCM count")
        .expect("seeded session and LCM count row")
        .get::<i64>(0)
        .expect("decode seeded session and LCM count")
}

async fn lcm_migration_applied_at(connection: &(impl QueryExecutor + ?Sized)) -> i64 {
    let mut rows = connection
        .query(
            "SELECT applied_at
             FROM session_schema_migrations
             WHERE name = 'lcm'",
            (),
        )
        .await
        .expect("read LCM migration applied_at");
    rows.next()
        .await
        .expect("read LCM migration row")
        .expect("LCM migration row")
        .get::<i64>(0)
        .expect("decode LCM migration applied_at")
}

fn accepting_memory_write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

async fn add_profile_schema_fact(
    database: &tracedecay_runtime_core::db::Database,
    label: &str,
) -> ProjectMemoryCurationMutationTarget {
    let memory = memory_application_for_db(FactOwnerV1::Profile, database)
        .expect("profile memory application");
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
        tracedecay_daemon_protocol::DaemonEndpoint::Unix(profile_root.join("session-runtime.sock"));
    #[cfg(not(unix))]
    let endpoint = tracedecay_daemon_protocol::default_loopback_endpoint();

    let first_authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        &profile_root,
        &endpoint,
        "test",
    )
    .expect("first daemon authority");
    let first_database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        first_authority.record().epoch,
        "first session runtime registry",
    )
    .expect("first daemon database scope");
    let identity = first_authority.profile_identity().clone();
    let first_registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("first session runtime registry");
    let stale = first_registry
        .profile_database()
        .await
        .expect("first profile authority client")
        .binding()
        .clone();
    assert_eq!(
        stale.incarnation.get(),
        first_authority.record().epoch,
        "the durable daemon generation must own the store incarnation"
    );
    drop(first_registry);
    drop(first_database_scope);
    drop(first_authority);

    let second_authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        &profile_root,
        &endpoint,
        "test",
    )
    .expect("successor daemon authority");
    let _second_database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        second_authority.record().epoch,
        "successor session runtime registry",
    )
    .expect("successor daemon database scope");
    let second_registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("successor session runtime registry");
    let current = second_registry
        .profile_database()
        .await
        .expect("successor profile authority client")
        .binding()
        .clone();

    assert_eq!(current.incarnation.get(), second_authority.record().epoch);
    assert!(current.incarnation > stale.incarnation);
    assert!(matches!(
        second_registry.lookup_store_runtime(&stale),
        tracedecay_runtime_core::store_runtime::registry::StoreRuntimeLookup::WrongIncarnation {
            expected,
            actual,
        } if *expected == stale && actual.as_ref() == &current
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn existing_profile_memory_uses_final_schema_and_canonical_linked_lineage() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "existing profile memory schema",
    )
    .expect("daemon database scope");
    let memory_path =
        tracedecay_runtime_core::memory::user::user_memory_db_path(identity.profile_root());
    let seed = TestConnection::open(&memory_path);
    tracedecay_runtime_core::db::migrations::create_schema_connection(&seed)
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
    let source = add_profile_schema_fact(&database, "source").await;
    let target = add_profile_schema_fact(&database, "target").await;
    let evidence = add_profile_schema_fact(&database, "evidence").await;
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
        .read_connection()
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
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "profile sessions identity pin",
    )
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_profile_sessions_mounts_singleflight_schema_admission() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "concurrent profile sessions mount",
    )
    .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
    let convergence_gate = registry.block_registered_schema_convergence_for_test();

    let (first, second, third, fourth) = tokio::join!(
        registry.profile_sessions(),
        registry.profile_sessions(),
        registry.profile_sessions(),
        registry.profile_sessions(),
    );
    let first = first.expect("first profile sessions mount");
    for mounted in [second, third, fourth] {
        let mounted = mounted.expect("concurrent profile sessions mount");
        assert!(!first.shares_client_with(&mounted));
        assert_eq!(first.binding(), mounted.binding());
    }
    convergence_gate.wait_until_blocked().await;
    assert_eq!(
        registry.registered_schema_convergence_schedule_count_for_test(),
        1,
        "one retained mount must schedule schema convergence exactly once"
    );
    assert_eq!(
        registry.registered_schema_convergence_execution_count_for_test(),
        1,
        "concurrent equivalent requests must execute convergence exactly once"
    );
    convergence_gate.release();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_node_mount_uses_registered_identity_and_reuses_one_runtime() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    #[cfg(unix)]
    let endpoint =
        tracedecay_daemon_protocol::DaemonEndpoint::Unix(profile_root.join("remote-runtime.sock"));
    #[cfg(not(unix))]
    let endpoint = tracedecay_daemon_protocol::default_loopback_endpoint();
    let daemon_authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        &profile_root,
        &endpoint,
        "test",
    )
    .expect("daemon authority");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
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
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
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
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "typed project sessions enrollment",
    )
    .expect("daemon database scope");
    let project_id = ProjectId::new("project.session-runtime").expect("typed project identity");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let sessions_path = tracedecay_runtime_core::storage::profile_sharded_data_root(
        identity.profile_root(),
        project_id.as_str(),
    )
    .join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);

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

    assert!(!first.shares_client_with(&second));
    assert_eq!(first.binding(), second.binding());
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
async fn daemon_admission_remains_ready_while_lcm_indexes_converge_in_background() {
    let (_temporary, identity, project_id, project_root, sessions_path, _database_scope) =
        project_sessions_pending_convergence("project.schema-admission").await;
    let seed = TestConnection::open(&sessions_path);
    seed.execute_batch(
        r"INSERT INTO sessions(provider, session_id, project_key, project_path)
           VALUES ('cursor', 'deferred-index-session', 'project.schema-admission', '/deferred');
           INSERT INTO lcm_raw_messages (
               provider, message_id, session_id, role, ordinal, content,
               content_hash, storage_kind, snippet_text, index_text,
               legacy_truncated, metadata_json
           ) VALUES (
               'cursor', 'deferred-index-message', 'deferred-index-session', 'assistant', 1,
               'deferred body', 'deferred-hash', 'inline', 'deferred', 'deferred', 1, NULL
           );
           UPDATE session_schema_migrations
               SET applied_at = 123
               WHERE name = 'lcm';
           DROP INDEX idx_lcm_raw_legacy_truncated;
           DROP INDEX idx_lcm_raw_lossy_ingest;
           DROP INDEX idx_lcm_summary_nodes_depth_tokens;
           DROP INDEX idx_lcm_external_payloads_owner_bytes;
           CREATE INDEX idx_lcm_external_payloads_owner
               ON lcm_external_payloads(provider, session_id);",
    )
    .await
    .expect("seed an already-current store without the LCM status indexes");
    drop(seed);
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
    let database = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::select! {
            result = &mut admission => {
                let database = result.expect("registered project sessions");
                convergence_blocked.await;
                database
            }
            () = &mut convergence_blocked => {
                admission.await.expect("registered project sessions")
            }
        }
    })
    .await
    .expect("daemon admission and convergence scheduling must not stall");

    assert_eq!(
        registry.registered_schema_convergence_status(&shard_id),
        Some(RegisteredSchemaConvergenceStatus::Running)
    );
    {
        let snapshot = database
            .read_snapshot()
            .await
            .expect("ordinary read snapshot while convergence is pending");
        let indexes = installed_lcm_status_index_names(&snapshot).await;
        for index in LCM_STATUS_PERFORMANCE_INDEX_NAMES {
            assert!(
                !indexes.iter().any(|installed| installed == index),
                "daemon admission synchronously built {index}: {indexes:?}"
            );
        }
        assert!(
            indexes
                .iter()
                .any(|index| index == SUPERSEDED_LCM_PAYLOAD_OWNER_INDEX),
            "daemon admission retired the old payload index before convergence: {indexes:?}"
        );
        assert_eq!(
            deferred_lcm_fixture_row_count(&snapshot).await,
            1,
            "ordinary session and LCM reads must remain available while convergence is pending"
        );
    }
    convergence_gate.release();
    assert_eq!(
        wait_for_schema_convergence(&registry, &shard_id).await,
        RegisteredSchemaConvergenceStatus::Complete
    );
    let snapshot = database
        .read_snapshot()
        .await
        .expect("ordinary read snapshot after convergence");
    let indexes = installed_lcm_status_index_names(&snapshot).await;
    for index in LCM_STATUS_PERFORMANCE_INDEX_NAMES {
        assert!(
            indexes.iter().any(|installed| installed == index),
            "background convergence did not build {index}: {indexes:?}"
        );
    }
    assert!(
        !indexes
            .iter()
            .any(|index| index == SUPERSEDED_LCM_PAYLOAD_OWNER_INDEX),
        "background convergence left the superseded payload index in place: {indexes:?}"
    );
    assert_eq!(
        deferred_lcm_fixture_row_count(&snapshot).await,
        1,
        "background convergence must preserve seeded session and LCM rows"
    );
    assert_eq!(
        lcm_migration_applied_at(&snapshot).await,
        123,
        "background convergence must not rewrite the current LCM migration marker"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_project_open_defers_historical_convergence_until_full_publication() {
    let (_temporary, identity, project_id, project_root, _sessions_path, _database_scope) =
        project_sessions_pending_convergence("project.schema-foreground-admission").await;
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
    let convergence_gate = registry.block_registered_schema_convergence_for_test();
    let foreground = registry
        .begin_foreground_project_open()
        .expect("foreground project-open admission");

    let database = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("registered project sessions publish for foreground open");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            convergence_gate.wait_until_blocked(),
        )
        .await
        .is_err(),
        "historical convergence must not enter its writer lane before full publication"
    );
    database
        .begin_write_transaction()
        .await
        .expect("foreground project-open writer is not queued behind convergence")
        .rollback()
        .await
        .expect("foreground project-open writer probe rolls back");

    drop(foreground);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        convergence_gate.wait_until_blocked(),
    )
    .await
    .expect("historical convergence starts after full publication");
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

    assert!(!first.shares_client_with(&second));
    assert_eq!(first.binding(), second.binding());
    assert_eq!(
        registry.registered_schema_convergence_schedule_count_for_test(),
        1,
        "the retained registry must deduplicate convergence tasks"
    );
    assert_eq!(
        registry.registered_schema_convergence_execution_count_for_test(),
        1,
        "the retained registry must execute the coalesced convergence once"
    );
    convergence_gate.release();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_convergence_runs_one_shard_while_other_shards_remain_pending() {
    let (_temporary, identity, project_id, project_root, _sessions_path, _database_scope) =
        project_sessions_pending_convergence("project.schema-concurrency").await;
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
    let convergence_gate = registry.block_registered_schema_convergence_for_test();

    let profile = registry
        .profile_sessions()
        .await
        .expect("registered profile sessions");
    convergence_gate.wait_until_blocked().await;
    let project = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("registered project sessions");
    let profile_shard = profile.binding().shard_id.clone();
    let project_shard = project.binding().shard_id.clone();

    assert_eq!(
        registry.registered_schema_convergence_status(&profile_shard),
        Some(RegisteredSchemaConvergenceStatus::Running),
        "the permit holder must be distinguishable from deferred work"
    );
    assert_eq!(
        registry.registered_schema_convergence_status(&project_shard),
        Some(RegisteredSchemaConvergenceStatus::Pending),
        "a second shard must wait without starting convergence"
    );
    assert_eq!(
        registry.registered_schema_convergence_schedule_count_for_test(),
        2,
        "both distinct shards must retain one scheduled task"
    );
    assert_eq!(
        registry.registered_schema_convergence_execution_count_for_test(),
        1,
        "only the permit holder may enter convergence"
    );

    convergence_gate.release();
    convergence_gate.release();
    assert_eq!(
        wait_for_schema_convergence(&registry, &profile_shard).await,
        RegisteredSchemaConvergenceStatus::Complete
    );
    assert_eq!(
        wait_for_schema_convergence(&registry, &project_shard).await,
        RegisteredSchemaConvergenceStatus::Complete
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_shutdown_cancels_and_joins_blocked_schema_convergence() {
    let (_temporary, identity, project_id, project_root, _sessions_path, _database_scope) =
        project_sessions_pending_convergence("project.schema-terminal-shutdown").await;
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
    let convergence_gate = registry.block_registered_schema_convergence_for_test();
    let _database = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("registered project sessions");
    convergence_gate.wait_until_blocked().await;

    registry.cancel_terminal_tasks();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        registry.shutdown_terminal_tasks(),
    )
    .await
    .expect("terminal shutdown must not wait for cancelled convergence work")
    .expect("session runtime terminal tasks shut down cleanly");
    convergence_gate.release();
    tokio::task::yield_now().await;

    assert_eq!(
        registry.registered_schema_convergence_status(&shard_id),
        Some(RegisteredSchemaConvergenceStatus::Running),
        "a cancelled convergence task must not publish a fabricated completion"
    );
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
async fn daemon_reopen_resumes_from_the_trusted_authority_checkpoint() {
    let (_temporary, identity, project_id, project_root, _sessions_path, _database_scope) =
        project_sessions_current_convergence("project.schema-trusted-checkpoint").await;
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
        1,
        "daemon reopen must audit only the suffix after a trusted checkpoint"
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
    tracedecay_global_db::schema_contract::ensure_authority_invariant_schema(&connection)
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
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "conflicting project sessions enrollment",
    )
    .expect("daemon database scope");
    let project_id = ProjectId::new("project.session-runtime").expect("typed project identity");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &first_project_root,
        project_id.as_str(),
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
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.non-git-worktree").expect("project id");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        13,
        "non-git worktree graph",
    )
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
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.generic-graph").expect("project id");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        19,
        "generic graph runtime",
    )
    .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_database = registry
        .project_memory(project_id.clone(), [project_root])
        .await
        .expect("project database");
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

    let publication_operation = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match project_database.issue_memory_graph_runtime_operation() {
                Ok(operation) => break operation,
                Err(
                    tracedecay_runtime_core::db::MemoryGraphRuntimeOperationErrorV1::Unbound
                    | tracedecay_runtime_core::db::MemoryGraphRuntimeOperationErrorV1::Unavailable,
                ) => {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("project graph attachment failed: {error:?}"),
            }
        }
    })
    .await
    .expect("project graph attachment becomes available");
    let published = publication_operation
        .runtime()
        .publish_verified_manifest(
            &manifest,
            idempotency.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("publish inline manifest");
    assert_eq!(published.generation(), &manifest.generation);
    let replayed = project_database
        .issue_memory_graph_runtime_operation()
        .expect("project graph replay operation")
        .runtime()
        .publish_verified_manifest(&manifest, idempotency, Arc::new(AtomicBool::new(false)))
        .expect("recover exact publication");
    assert_eq!(replayed.generation(), &manifest.generation);
    let recovered = project_database
        .issue_memory_graph_runtime_operation()
        .expect("project graph snapshot operation")
        .runtime()
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
    project_database
        .issue_memory_graph_runtime_operation()
        .expect("project graph successor operation")
        .runtime()
        .publish_verified_manifest(
            &successor,
            GraphIdempotencyKey::new("idempotency.generic-test.2").expect("successor idempotency"),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("publish successor");
    assert!(matches!(
        project_database
            .issue_memory_graph_runtime_operation()
            .expect("project graph stale-publication operation")
            .runtime()
            .publish_verified_manifest(
                &manifest,
                GraphIdempotencyKey::new("idempotency.generic-test.1").expect("stale idempotency"),
                Arc::new(AtomicBool::new(false)),
            ),
        Err(GraphDbError::Conflict { .. })
    ));

    let cancelled = FactReadControl::new(Arc::new(|| true));
    assert!(matches!(
        project_database
            .issue_memory_graph_runtime_operation()
            .expect("project graph cancelled-read operation")
            .runtime()
            .verified_snapshot(&projection, cancelled),
        Err(GraphDbError::Cancelled)
    ));
    let missing = GraphProjectionIdentity::new(
        projection.namespace.clone(),
        GraphProjectionId::new("projection.generic-test.missing").expect("missing projection"),
    );
    // Never published: the typed empty start, not an unavailability error.
    assert!(matches!(
        project_database
            .issue_memory_graph_runtime_operation()
            .expect("project graph empty-read operation")
            .runtime()
            .verified_snapshot(&missing, FactReadControl::new(Arc::new(|| false))),
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
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.shared-code-graph").expect("project id");
    for project_root in [&primary_root, &linked_root] {
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            project_root,
            project_id.as_str(),
        )
        .expect("project enrollment");
    }
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        17,
        "shared code graph",
    )
    .expect("daemon database scope");
    let registry = Arc::new(
        DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry"),
    );
    let project_database = registry
        .project_memory(
            project_id.clone(),
            [primary_root.clone(), linked_root.clone()],
        )
        .await
        .expect("mount project authority");
    let repository_id = tracedecay_domain::RepositoryId::new("repo:shared").expect("repository id");
    let replay_binding = || tracedecay_code_index_runtime::CodeGraphReplayBindingV1 {
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
        .code_graph_seat_port()
        .retain_code_graph_runtime(
            project_id.clone(),
            repository_id.clone(),
            tracedecay_domain::WorktreeId::new("worktree:primary").expect("primary worktree"),
            None,
            tracedecay_domain::CodeGenerationId::new("generation:primary")
                .expect("primary generation"),
            Arc::clone(&project_database),
            replay_binding(),
            None,
        )
        .await
        .expect("primary graph runtime");
    let linked = registry
        .code_graph_seat_port()
        .retain_code_graph_runtime(
            project_id,
            repository_id,
            tracedecay_domain::WorktreeId::new("worktree:linked").expect("linked worktree"),
            None,
            tracedecay_domain::CodeGenerationId::new("generation:linked")
                .expect("linked generation"),
            Arc::clone(&project_database),
            replay_binding(),
            None,
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
async fn corrupt_derived_graph_preserves_relational_owner_lifecycle() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    gix::init(&project_root).expect("initialize project repository");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.derived-graph-corrupt").expect("project id");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        23,
        "corrupt derived graph project open",
    )
    .expect("daemon database scope");
    let project_store_root = profile_root.join("projects/project.derived-graph-corrupt");
    let database_path = project_store_root.join(crate::config::db_filename(&project_store_root));
    std::fs::create_dir_all(database_path.parent().expect("database parent"))
        .expect("database directory");

    let first_registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("first session runtime registry");
    first_registry
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("register project authority");
    let authority = DatabaseAuthority::for_runtime(
        &database_path,
        "seed project store before derived graph corruption",
    )
    .expect("project database authority");
    let first_database = first_registry
        .project_graph(
            &project_root,
            project_id.clone(),
            database_path.clone(),
            authority,
            DatabaseAccessMode::ReadWrite,
        )
        .await
        .expect("initial project graph publication");
    let graph_path = first_database.database_path().with_extension("grafeo");
    drop(first_database);
    first_registry.cancel_terminal_tasks();
    first_registry
        .shutdown_terminal_tasks()
        .await
        .expect("first terminal tasks shut down");
    first_registry.cancel_memory_graph_reconciliation_tasks();
    first_registry
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("first reconciliation shuts down");
    first_registry
        .close_retained_graph_runtimes_for_shutdown()
        .await
        .expect("first graph runtime closes");
    drop(first_registry);

    std::fs::write(&graph_path, b"corrupt derived graph").expect("corrupt derived graph file");

    let reopened_registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("reopened session runtime registry");
    reopened_registry
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("restore project authority");
    let reopened_authority = DatabaseAuthority::for_runtime(
        &database_path,
        "reopen project store with corrupt derived graph",
    )
    .expect("reopened project database authority");
    let reopened = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        reopened_registry.project_graph(
            &project_root,
            project_id.clone(),
            database_path.clone(),
            reopened_authority,
            DatabaseAccessMode::ReadWrite,
        ),
    )
    .await
    .expect("relational project open must not wait for derived graph recovery")
    .expect("relational project open remains available");
    assert_eq!(reopened.database_path(), database_path);
    // The relational open never waits for the derived graph: the memory-graph
    // attachment is a retained background task, so the operation reads
    // `Unbound` until that task settles. Recovery then rebuilds the corrupt
    // derived graph and binds it -- on Linux roughly one tick after the mount
    // returns, on macOS before the first read -- so asserting the warm-up
    // window is asserting a race. Wait for the settled outcome instead, and
    // keep refusing the one result that would mean the relational owner was
    // damaged.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match reopened.issue_memory_graph_runtime_operation() {
                Ok(_) => return,
                Err(
                    tracedecay_runtime_core::db::MemoryGraphRuntimeOperationErrorV1::Unbound
                    | tracedecay_runtime_core::db::MemoryGraphRuntimeOperationErrorV1::Unavailable,
                ) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                Err(error) => {
                    panic!("corrupt derived graph must not fault the relational owner: {error:?}")
                }
            }
        }
    })
    .await
    .expect("derived graph recovery rebinds the memory graph to its relational owner");
    drop(reopened);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        reopened_registry.retire_project_memory_graph(&project_id),
    )
    .await
    .expect("relational owner retirement must not wait for derived graph recovery")
    .expect("detached relational owner retires cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_session_relation_graph_preserves_relational_session_database() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    gix::init(&project_root).expect("initialize project repository");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.session-relation-corrupt").expect("project id");

    let first_registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("first session runtime registry");
    let first_database = first_registry
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("initial project session database");
    let database_path = first_database.db_path().to_path_buf();
    let graph_path = database_path.with_extension("grafeo");
    drop(first_database);
    first_registry.cancel_terminal_tasks();
    first_registry
        .shutdown_terminal_tasks()
        .await
        .expect("first terminal tasks shut down");
    first_registry
        .close_retained_graph_runtimes_for_shutdown()
        .await
        .expect("first session relation graph closes");
    drop(first_registry);

    std::fs::write(&graph_path, b"corrupt session relation graph")
        .expect("corrupt session relation graph file");

    let reopened_registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("reopened session runtime registry");
    let reopened = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        reopened_registry.project_sessions(project_id.clone(), [project_root]),
    )
    .await
    .expect("relational session open must not wait for relation graph recovery")
    .expect("relational session database remains available");
    assert_eq!(
        reopened.db_path(),
        database_path.as_path(),
        "the corrupt relation graph must not displace the relational session owner"
    );
    // The relational open never waits for the relation graph: the attachment
    // is a retained background task, so a lease issued the instant the mount
    // returns reads the warming window and reports the graph unavailable.
    // Recovery then rebuilds the corrupt graph and binds it into the same
    // registered owner every lease shares -- on Linux a tick after the mount
    // returns, on macOS before this first read -- so asserting the warming
    // window is asserting a race. Settle the task through the typed signal
    // that exists for exactly this fixture, then assert the terminal outcome.
    reopened_registry
        .settle_project_session_graph(&project_id)
        .await
        .expect("session relation graph settles after corrupt-file recovery");
    assert!(
        reopened.session_relation_graph_identity().is_ok(),
        "recovery must rebind a rebuilt relation graph to the intact relational owner"
    );
    drop(reopened);
    reopened_registry.cancel_terminal_tasks();
    reopened_registry
        .shutdown_terminal_tasks()
        .await
        .expect("detached session relation graph task shuts down");
    reopened_registry
        .close_retained_graph_runtimes_for_shutdown()
        .await
        .expect("detached relational session owner shuts down cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_session_relation_graph_reaches_the_published_relational_lease() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    gix::init(&project_root).expect("initialize project repository");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.session-relation-late-bind").expect("project id");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");

    let sessions = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("relational sessions publish before graph restore");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while sessions.session_relation_graph_identity().is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the published relational lease observes its late graph attachment");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_project_memory_graph_reaches_the_published_session_lease() {
    let temporary = tempfile::tempdir().expect("temporary project parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    gix::init(&project_root).expect("initialize project repository");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let project_id = ProjectId::new("project.memory-relation-late-bind").expect("project id");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");

    let sessions = registry
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("relational sessions publish before memory graph restore");
    registry
        .project_memory(project_id, [project_root])
        .await
        .expect("project memory database publishes before graph restore");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while sessions.project_graph_runtime().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the published session lease observes its late project graph attachment");
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
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        11,
        "project graph publication",
    )
    .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_id = ProjectId::new("project.graph-publication").expect("project id");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
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
        tracedecay_runtime_core::db::DatabaseAuthorityRole::Daemon
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
    let publication_id = main.runtime_client().publication().publication_id;
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
        read_only.runtime_client().publication().publication_id,
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
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("durable profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
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

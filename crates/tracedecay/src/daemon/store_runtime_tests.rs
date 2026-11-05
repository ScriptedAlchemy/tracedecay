//! Composition-root integration tests for the store-runtime crate.
//!
//! Shared root runtime-port fixtures remain here; registry lifecycle tests live
//! in `tracedecay-store-runtime`.

use std::path::PathBuf;
use std::sync::Arc;
use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{
    BrainNodeId, Confidence, FactCategoryV1, FactCurationActionV1, FactLineageEventKindV1,
    FactOwnerV1, FactRelationKindV1,
};
use tracedecay_global_db::register_registered_schema_installer;
use tracedecay_runtime_core::db::engine::{QueryExecutor, TestConnection};
use tracedecay_runtime_core::db::{DatabaseAccessMode, DatabaseAuthority};
use tracedecay_rusqlite_runtime::remote::{
    RemoteSpoolKeyV1, RemoteSpoolKeyringV1, RemoteSqliteStorageErrorV1,
};
use tracedecay_session_memory::memory::{
    MemoryOperationContext, ProjectMemoryCurationMutationTarget, ProjectMemoryCurationOperation,
    ProjectMemoryFactAddRequest, ProjectMemoryFactAddRequestOutcome, memory_application_for_db,
};
use tracedecay_store::{
    FactReadControl, FactWriteControl, ProjectId, ProjectMemoryFactHistoryQueryV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1, StoreShardIdV1,
};
use tracedecay_store_runtime::{DaemonSessionRuntimeRegistryV1, RegisteredSchemaConvergenceStatus};

/// Bound for the assertions in this file that read "must not wait for
/// background recovery" or "must settle".
///
/// The regression each one catches is an operation that never returns, and no
/// finite bound reaches that, so the value only has to exceed the honest
/// latency of the operation itself. At two seconds it did not: a machine
/// running the whole lib suite across its cores opens a session database
/// slower than that, and the bound reported a loaded host as a blocked one.
const MUST_NOT_BLOCK: std::time::Duration = std::time::Duration::from_mins(1);

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
    let fixture = tracedecay_runtime_core::db::Database::publish_registered_test_runtime_with_retirement_control_for_profile_identity(
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
    let (owner, runtime, retirement) = fixture.into_parts();
    let database = owner
        .issue_lease()
        .expect("seed project sessions fixture lease");
    if remove_checkpoint {
        database
            .execute_write_batch(
                "remove durable convergence checkpoint",
                "DELETE FROM authority_audit_checkpoints",
            )
            .await
            .expect("remove durable convergence checkpoint");
    }
    // Dropping the last lease does not always close the store before this
    // returns: another holder of the fixture registry can outlive it, and its
    // later close checkpoints the WAL while the caller already writes with a
    // zero busy timeout. Close it through its registry and await the close.
    drop((database, owner, runtime));
    retirement
        .registry()
        .close_path(&sessions_path)
        .await
        .expect("close the seeded project sessions runtime")
        .expect("the seeded project sessions runtime is registered");
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

/// Bound on an attempt failing behind a held write lock, and on the attempt
/// after the lock clears committing. A failing attempt lasts the writer's whole
/// busy timeout.
const RECOVERY_OBSTRUCTION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);

/// [`wait_for_schema_convergence`] for an attempt that first has to outlast the
/// writer's busy timeout behind a held lock.
async fn wait_for_schema_convergence_past_obstruction(
    registry: &DaemonSessionRuntimeRegistryV1,
    shard_id: &StoreShardIdV1,
) -> RegisteredSchemaConvergenceStatus {
    tokio::time::timeout(RECOVERY_OBSTRUCTION_DEADLINE, async {
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
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the obstructed attempt must reach a terminal state")
}

/// The `SQLite` scalar one query returns, for the fixtures that assert on a
/// single count or marker.
async fn scalar_i64(connection: &(impl QueryExecutor + ?Sized), sql: &str) -> i64 {
    let mut rows = connection.query(sql, ()).await.expect("read scalar");
    rows.next()
        .await
        .expect("step scalar")
        .expect("scalar row")
        .get::<i64>(0)
        .expect("decode scalar")
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
        tracedecay_session_memory::memory::user::user_memory_db_path(identity.profile_root());
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
    let grant = tracedecay_store_runtime::remote_enrollment_grant_fixture(
        identity.brain_id().clone(),
        node_id.clone(),
        &secret,
    );
    registry
        .provision_remote_node(
            grant.clone(),
            tracedecay_store_runtime::remote_enrollment_admission_fixture(&grant),
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

async fn migrating_session_row_count(connection: &(impl QueryExecutor + ?Sized)) -> i64 {
    scalar_i64(
        connection,
        "SELECT COUNT(*) FROM sessions WHERE session_id = 'migrating-session'",
    )
    .await
}

/// Store-sized convergence runs after admission, never inside the open's
/// leased schema transaction, where a large store outran its execution
/// deadline and the daemon restarted into the same failure forever.
///
/// Admission must therefore complete, the store must serve while
/// convergence is outstanding, Doctor must report which store is converging
/// rather than claiming health, and background convergence must settle it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_convergence_is_reported_and_completes_after_admission() {
    let (_temporary, identity, project_id, project_root, sessions_path, _database_scope) =
        project_sessions_pending_convergence("project.schema-store-sized").await;
    let seed = TestConnection::open(&sessions_path);
    seed.execute_batch(
        r"INSERT INTO sessions(provider, session_id, project_key, project_path)
           VALUES ('cursor', 'migrating-session', 'project.schema-store-sized', '/migrating');",
    )
    .await
    .expect("seed a store row");
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

    let database = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("admission must complete on a store with migrations outstanding");
    convergence_gate.wait_until_blocked().await;

    {
        let snapshot = database
            .read_snapshot()
            .await
            .expect("ordinary read snapshot while migrations are outstanding");
        assert_eq!(
            migrating_session_row_count(&snapshot).await,
            1,
            "the store must serve ordinary reads while its migrations are outstanding"
        );
    }

    let unconverged = registry.unconverged_registered_schemas();
    assert_eq!(
        unconverged,
        vec![(shard_id.clone(), RegisteredSchemaConvergenceStatus::Running)],
        "the shard still converging must be the one reported"
    );
    let convergence_observations = registry.registered_schema_convergence_observations();
    let [convergence] = convergence_observations.as_slice() else {
        panic!("one running store must publish one typed convergence finding");
    };
    assert_eq!(
        convergence.state,
        tracedecay_contracts::storage::SchemaConvergenceStateV1::Running
    );
    assert!(convergence.started_at_micros > 0);
    let report =
        tracedecay_daemon_service::doctor_kernel::schema_convergence_incomplete_read(&unconverged);
    let tracedecay_contracts::doctor::DoctorStorageFamilyReadV1::Observed { findings } = report
    else {
        panic!("a store mid-convergence must be reported, not omitted: {report:?}");
    };
    let [finding] = findings.as_slice() else {
        panic!("one outstanding shard is one finding: {findings:?}");
    };
    assert_eq!(
        finding.kind(),
        tracedecay_contracts::doctor::DoctorStorageFindingKindV1::SchemaConvergenceIncomplete
    );
    assert_eq!(
        finding.finding().state(),
        tracedecay_contracts::doctor::DoctorEvidenceStateV1::Stale,
        "a readable store still converging is stale, never healthy"
    );

    convergence_gate.release();
    assert_eq!(
        wait_for_schema_convergence(&registry, &shard_id).await,
        RegisteredSchemaConvergenceStatus::Complete
    );
    assert!(
        registry.unconverged_registered_schemas().is_empty(),
        "a converged shard must stop being reported"
    );
    assert_eq!(
        registry.registered_schema_convergence_observations()[0].state,
        tracedecay_contracts::storage::SchemaConvergenceStateV1::Completed
    );
    let snapshot = database
        .read_snapshot()
        .await
        .expect("ordinary read snapshot after convergence");
    assert_eq!(
        migrating_session_row_count(&snapshot).await,
        1,
        "convergence must preserve the seeded rows"
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
    tokio::time::timeout(MUST_NOT_BLOCK, convergence_gate.wait_until_blocked())
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
        MUST_NOT_BLOCK,
        registry.shutdown_terminal_tasks(&tracedecay_runtime_core::native_runtime::cx().unwrap()),
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

/// The artifact table as every release through `v1.0.0-beta.92` defined it.
const PUBLISHED_WORKFLOW_ARTIFACT_TABLE_SQL: &str = "CREATE TABLE workflow_artifact_payloads (
    payload_digest TEXT NOT NULL PRIMARY KEY,
    byte_length INTEGER NOT NULL CHECK (
        byte_length = length(payload) AND byte_length <= 4194304
    ),
    payload BLOB NOT NULL
) STRICT";
pub(crate) const PUBLISHED_WORKFLOW_DIGEST: &str =
    "sha256:a292df6bc47e763f0d20bdb44a4032c0b1d7ac8e4cb83173b66ae7d1ff0d03be";

pub(crate) fn age_session_store_to_published_workflow_shape(sessions_path: &std::path::Path) {
    rusqlite::Connection::open(sessions_path)
        .expect("open the stopped session store")
        .execute_batch(&format!(
            "DROP TABLE workflow_artifact_payloads;
             {PUBLISHED_WORKFLOW_ARTIFACT_TABLE_SQL};
             UPDATE workflow_schema SET definition_digest = '{PUBLISHED_WORKFLOW_DIGEST}';
             INSERT INTO workflow_artifact_payloads VALUES ('sha256:kept', 3, x'010203');"
        ))
        .expect("give the store the published workflow shape");
}

/// What an outside reader finds: the identity digest and the artifact rows.
pub(crate) fn published_workflow_state(
    sessions_path: &std::path::Path,
) -> (String, Vec<(String, Vec<u8>)>) {
    let connection = rusqlite::Connection::open(sessions_path).expect("open the session store");
    let digest = connection
        .query_row("SELECT definition_digest FROM workflow_schema", [], |row| {
            row.get(0)
        })
        .expect("read the workflow identity");
    let rows = connection
        .prepare("SELECT payload_digest, payload FROM workflow_artifact_payloads")
        .expect("prepare the artifact read")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("read the artifact rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("decode the artifact rows");
    (digest, rows)
}

/// The published upgrade is historical convergence: the shard mounts, its other
/// authorities serve, repeated mounts schedule it once, and only workflow
/// storage answers typed recovering until the background worker has committed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn published_workflow_shape_recovers_behind_a_serving_session_store() {
    let (_temporary, identity, project_id, project_root, sessions_path, _database_scope) =
        project_sessions_convergence_fixture("project.published-workflow-recovery", false).await;
    age_session_store_to_published_workflow_shape(&sessions_path);
    let published = published_workflow_state(&sessions_path);
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
    let convergence_gate = registry.block_registered_schema_convergence_for_test();

    let database = tokio::time::timeout(
        MUST_NOT_BLOCK,
        registry.project_sessions(project_id.clone(), [project_root.clone()]),
    )
    .await
    .expect("mounting must not wait for the artifact copy")
    .expect("a published store admits");
    convergence_gate.wait_until_blocked().await;

    assert_eq!(published_workflow_state(&sessions_path), published);
    let (again, store) = tokio::join!(
        registry.project_sessions(project_id.clone(), [project_root.clone()]),
        registry.project_session_store(project_id.clone(), [project_root]),
    );
    let again = again.expect("a recovering store keeps serving session clients");
    store.expect("a recovering store keeps serving its other authorities");
    assert!(
        registry
            .mounted_project_sessions(&project_id)
            .await
            .is_some(),
        "only workflow storage waits for the upgrade"
    );
    assert_eq!(
        registry.registered_schema_convergence_schedule_count_for_test(),
        1,
        "concurrent admission must schedule the upgrade once"
    );
    assert!(
        registry.reset_required_stores().is_empty(),
        "a transient failure is not a reset"
    );
    let finding = registry
        .registered_schema_convergence_observations()
        .into_iter()
        .next()
        .expect("the running upgrade is observable");
    assert_eq!(
        finding.state,
        tracedecay_contracts::storage::SchemaConvergenceStateV1::Running
    );
    for client in [&database, &again] {
        let refused = client
            .workflow_storage()
            .await
            .err()
            .expect("workflow storage waits for the upgrade");
        assert!(refused.is_workflow_authority_recovering(), "{refused}");
    }
    let mut served = database
        .read_connection()
        .query("SELECT count(*) FROM parse_offsets", ())
        .await
        .expect("the store's other authorities read while recovering");
    served
        .next()
        .await
        .expect("read the count")
        .expect("a count row");

    convergence_gate.release();
    assert_eq!(
        wait_for_schema_convergence(&registry, &shard_id).await,
        RegisteredSchemaConvergenceStatus::Complete
    );
    database
        .workflow_storage()
        .await
        .expect("a client issued before the commit serves after it");
    let (digest, rows) = published_workflow_state(&sessions_path);
    assert_ne!(digest, PUBLISHED_WORKFLOW_DIGEST);
    assert_eq!(rows, published.1);
}

/// A peer holding the store's write lock fails the upgrade. Nothing reopens the
/// store: its one convergence task tries again until the lock clears, each
/// failed attempt rolled back and the store typed recovering rather than reset.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn published_workflow_recovery_outlasts_a_transient_lock_without_a_reopen() {
    let (_temporary, identity, project_id, project_root, sessions_path, _database_scope) =
        project_sessions_convergence_fixture("project.published-workflow-retry", false).await;
    age_session_store_to_published_workflow_shape(&sessions_path);
    let published = published_workflow_state(&sessions_path);
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
    let convergence_gate = registry.block_registered_schema_convergence_for_test();
    let database = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("a published store admits");
    convergence_gate.wait_until_blocked().await;
    let attempts_before = registry.registered_schema_convergence_execution_count_for_test();

    let peer = rusqlite::Connection::open(&sessions_path).expect("open the session store");
    peer.execute_batch("BEGIN IMMEDIATE")
        .expect("hold the write lock");
    convergence_gate.retry_on_timer(std::time::Duration::from_millis(20));
    convergence_gate.release();

    tokio::time::timeout(RECOVERY_OBSTRUCTION_DEADLINE, async {
        while registry.registered_schema_convergence_execution_count_for_test()
            < attempts_before + 2
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a failed attempt must be followed by another one");
    assert_eq!(
        registry.registered_schema_convergence_schedule_count_for_test(),
        1,
        "retries belong to the one scheduled task"
    );
    assert!(
        registry.reset_required_stores().is_empty(),
        "a transient failure is not a reset"
    );
    let refused = database
        .workflow_storage()
        .await
        .err()
        .expect("nothing committed");
    assert!(refused.is_workflow_authority_recovering(), "{refused}");
    assert_eq!(published_workflow_state(&sessions_path), published);

    peer.execute_batch("ROLLBACK").expect("drop the write lock");
    drop(peer);
    tokio::time::timeout(RECOVERY_OBSTRUCTION_DEADLINE, async {
        while registry.registered_schema_convergence_status(&shard_id)
            != Some(RegisteredSchemaConvergenceStatus::Complete)
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the attempt after the lock clears must commit");
    database
        .workflow_storage()
        .await
        .expect("a client issued before the failure serves after the retry commits");
    let (digest, rows) = published_workflow_state(&sessions_path);
    assert_ne!(digest, PUBLISHED_WORKFLOW_DIGEST);
    assert_eq!(rows, published.1);
}

/// A store waiting for its next attempt holds no writer and no admission
/// permit, so shutdown ends the wait at once and the restart resumes the move.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_ends_the_wait_between_published_workflow_attempts() {
    let (_temporary, identity, project_id, project_root, sessions_path, _database_scope) =
        project_sessions_convergence_fixture("project.published-workflow-retry-stop", false).await;
    age_session_store_to_published_workflow_shape(&sessions_path);
    let published = published_workflow_state(&sessions_path);
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
    let convergence_gate = registry.block_registered_schema_convergence_for_test();
    let database = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("a published store admits");
    convergence_gate.wait_until_blocked().await;

    let peer = rusqlite::Connection::open(&sessions_path).expect("open the session store");
    peer.execute_batch("BEGIN IMMEDIATE")
        .expect("hold the write lock");
    convergence_gate.release();
    let status = wait_for_schema_convergence_past_obstruction(&registry, &shard_id).await;
    assert!(
        matches!(status, RegisteredSchemaConvergenceStatus::Retrying { .. }),
        "the held lock must fail the attempt and leave it retrying: {status:?}"
    );
    peer.execute_batch("ROLLBACK").expect("drop the write lock");
    drop(peer);
    let attempts = registry.registered_schema_convergence_execution_count_for_test();

    registry.cancel_terminal_tasks();
    tokio::time::timeout(
        MUST_NOT_BLOCK,
        registry.shutdown_terminal_tasks(&tracedecay_runtime_core::native_runtime::cx().unwrap()),
    )
    .await
    .expect("shutdown must not wait out the delay between attempts")
    .expect("session runtime terminal tasks shut down cleanly");
    convergence_gate.release();
    tokio::task::yield_now().await;

    assert_eq!(
        registry.registered_schema_convergence_execution_count_for_test(),
        attempts,
        "a stopped daemon starts no further attempt"
    );
    assert!(
        database
            .workflow_storage()
            .await
            .err()
            .expect("nothing committed")
            .is_workflow_authority_recovering()
    );
    assert_eq!(published_workflow_state(&sessions_path), published);
}

/// The doctor's evidence reference for the one store still converging.
fn schema_convergence_doctor_evidence(registry: &DaemonSessionRuntimeRegistryV1) -> String {
    let report = tracedecay_daemon_service::doctor_kernel::schema_convergence_incomplete_read(
        &registry.unconverged_registered_schemas(),
    );
    let tracedecay_contracts::doctor::DoctorStorageFamilyReadV1::Observed { findings } = report
    else {
        panic!("an unconverged store must be reported: {report:?}");
    };
    let [finding] = findings.as_slice() else {
        panic!("one unconverged store is one finding: {findings:?}");
    };
    assert_eq!(
        finding.finding().state(),
        tracedecay_contracts::doctor::DoctorEvidenceStateV1::Degraded
    );
    finding.finding().evidence()[0]
        .reference()
        .as_str()
        .to_owned()
}

/// A mounted published store whose first upgrade attempt failed behind a real
/// write lock, parked between attempts with the lock released, so nothing but
/// the waiting task stands between it and the next attempt.
struct PublishedStoreBetweenAttempts {
    _temporary: tempfile::TempDir,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _session_sync: Arc<tracedecay_session_runtime::session_sync::DaemonSessionSyncService>,
    registry: DaemonSessionRuntimeRegistryV1,
    gate: tracedecay_store_runtime::RegisteredSchemaConvergenceTestGate,
    database: Option<tracedecay_global_db::RegisteredGlobalDbLeaseV1>,
    project_id: ProjectId,
    project_root: PathBuf,
    sessions_path: PathBuf,
    shard_id: StoreShardIdV1,
    published: (String, Vec<(String, Vec<u8>)>),
}

async fn published_store_between_attempts(
    project_name: &str,
    detached_session_graph: bool,
) -> PublishedStoreBetweenAttempts {
    let (temporary, identity, project_id, project_root, sessions_path, database_scope) =
        project_sessions_convergence_fixture(project_name, false).await;
    age_session_store_to_published_workflow_shape(&sessions_path);
    let published = published_workflow_state(&sessions_path);
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
    let session_sync =
        Arc::new(tracedecay_session_runtime::session_sync::DaemonSessionSyncService::default());
    registry
        .install_session_sync_service(&session_sync)
        .expect("install session sync lifecycle authority");
    if detached_session_graph {
        registry.detach_session_graphs_for_test();
    }
    let gate = registry.block_registered_schema_convergence_for_test();
    let database = registry
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("a published store admits");
    gate.wait_until_blocked().await;
    let peer = rusqlite::Connection::open(&sessions_path).expect("open the session store");
    peer.execute_batch("BEGIN IMMEDIATE")
        .expect("hold the write lock");
    gate.release();
    let status = wait_for_schema_convergence_past_obstruction(&registry, &shard_id).await;
    assert!(
        matches!(status, RegisteredSchemaConvergenceStatus::Retrying { .. }),
        "a held lock leaves the upgrade retrying: {status:?}"
    );
    peer.execute_batch("ROLLBACK").expect("drop the write lock");
    drop(peer);
    registry
        .settle_project_session_graph(&project_id)
        .await
        .expect("the session graph settles");
    PublishedStoreBetweenAttempts {
        _temporary: temporary,
        _database_scope: database_scope,
        _session_sync: session_sync,
        registry,
        gate,
        database: Some(database),
        project_id,
        project_root,
        sessions_path,
        shard_id,
        published,
    }
}

impl PublishedStoreBetweenAttempts {
    async fn assert_mounted_again_finishes_the_upgrade(&self) {
        let schedules = self
            .registry
            .registered_schema_convergence_schedule_count_for_test();
        let attempts = self
            .registry
            .registered_schema_convergence_execution_count_for_test();
        let database = self
            .registry
            .project_sessions(self.project_id.clone(), [self.project_root.clone()])
            .await
            .expect("a store mounted again admits");
        assert_eq!(
            self.registry
                .registered_schema_convergence_schedule_count_for_test(),
            schedules + 1,
            "the new owner must get a worker of its own"
        );
        self.gate.release();
        assert_eq!(
            wait_for_schema_convergence_past_obstruction(&self.registry, &self.shard_id).await,
            RegisteredSchemaConvergenceStatus::Complete
        );
        assert_eq!(
            self.registry
                .registered_schema_convergence_execution_count_for_test(),
            attempts + 1,
            "only the new worker attempts the upgrade"
        );
        database
            .workflow_storage()
            .await
            .expect("the mount's own worker finished the upgrade");
        let (digest, rows) = published_workflow_state(&self.sessions_path);
        assert_ne!(digest, PUBLISHED_WORKFLOW_DIGEST);
        assert_eq!(rows, self.published.1);
    }
}

/// The doctor names a failed attempt that will retry by itself as retrying, with
/// its failure, rather than as stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_reports_a_store_between_upgrade_attempts_as_retrying() {
    let store =
        published_store_between_attempts("project.published-workflow-doctor-retrying", false).await;
    let evidence = schema_convergence_doctor_evidence(&store.registry);
    assert!(evidence.contains(".retrying."), "{evidence}");
    assert!(!evidence.contains("stopped"), "{evidence}");
    let finding = store
        .registry
        .registered_schema_convergence_observations()
        .into_iter()
        .next()
        .expect("the waiting store is observable");
    assert_eq!(
        finding.state,
        tracedecay_contracts::storage::SchemaConvergenceStateV1::Retrying
    );
    assert!(finding.degraded_row.is_some(), "the failure stays visible");
}

/// Retiring a store between attempts ends the wait at once, whatever is left of
/// it: the gate never lets this wait finish. Its status leaves with it, and
/// mounting the store again starts a worker over the new owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retiring_a_store_between_upgrade_attempts_ends_the_wait() {
    let mut store =
        published_store_between_attempts("project.published-workflow-retry-retire", false).await;
    let attempts = store
        .registry
        .registered_schema_convergence_execution_count_for_test();
    retire_published_store(&mut store).await;
    assert_eq!(
        store
            .registry
            .registered_schema_convergence_status(&store.shard_id),
        None,
        "a retired store reports nothing"
    );
    assert_eq!(
        store.registry.registered_schema_convergence_observations(),
        Vec::new()
    );
    assert_eq!(
        store
            .registry
            .registered_schema_convergence_execution_count_for_test(),
        attempts,
        "the retired wait starts no attempt"
    );
    store.assert_mounted_again_finishes_the_upgrade().await;
}

/// Closes the store through its registry. The fixture's own client is the only
/// lease that could block this: the waiting task holds none.
async fn retire_published_store(store: &mut PublishedStoreBetweenAttempts) {
    store.database = None;
    tokio::time::timeout(
        MUST_NOT_BLOCK,
        store
            .registry
            .retire_project_session_relation_graph(&store.project_id),
    )
    .await
    .expect("retirement must not wait out the delay between attempts")
    .expect("a store between attempts retires");
    assert!(
        store
            .registry
            .mounted_project_sessions(&store.project_id)
            .await
            .is_none()
    );
}

/// A retirement that does not retire the worker leaves it waiting on an owner
/// that is gone. Mounting the store again must not trust that worker: only a new
/// one can finish the upgrade.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_remount_replaces_the_worker_of_a_store_still_retrying() {
    let mut store =
        published_store_between_attempts("project.published-workflow-remount-retrying", true).await;
    retire_published_store(&mut store).await;
    assert!(
        matches!(
            store
                .registry
                .registered_schema_convergence_status(&store.shard_id),
            Some(RegisteredSchemaConvergenceStatus::Retrying { .. })
        ),
        "this retirement path leaves the worker's status behind"
    );
    store.assert_mounted_again_finishes_the_upgrade().await;
}

/// A worker that wakes to find its owner gone stops claiming to retry, and a
/// mount over that stopped upgrade still gets a worker of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_remount_replaces_the_worker_of_a_previously_failed_upgrade() {
    let mut store =
        published_store_between_attempts("project.published-workflow-remount-failed", true).await;
    let attempts = store
        .registry
        .registered_schema_convergence_execution_count_for_test();
    retire_published_store(&mut store).await;
    store.gate.release();
    let message = tokio::time::timeout(MUST_NOT_BLOCK, async {
        loop {
            if let Some(RegisteredSchemaConvergenceStatus::Degraded { message }) = store
                .registry
                .registered_schema_convergence_status(&store.shard_id)
            {
                return message;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a worker whose owner is gone must stop claiming to retry");
    assert!(message.contains("mounting it again"), "{message}");
    assert_eq!(
        store
            .registry
            .registered_schema_convergence_execution_count_for_test(),
        attempts,
        "nothing can attempt the upgrade of a closed store"
    );
    let evidence = schema_convergence_doctor_evidence(&store.registry);
    assert!(evidence.contains(".stopped."), "{evidence}");
    assert!(!evidence.contains(".retrying."), "{evidence}");
    store.assert_mounted_again_finishes_the_upgrade().await;
}

/// A retirement refused by an outstanding client rolls back to the same owner.
/// It already stopped the worker, so without a new one the mounted store would
/// answer recovering until the daemon restarted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rolled_back_retirement_restarts_the_interrupted_upgrade() {
    let store =
        published_store_between_attempts("project.published-workflow-retire-rollback", false).await;
    let client = store
        .database
        .as_ref()
        .expect("the fixture's client")
        .clone();
    let refusal = store
        .registry
        .retire_project_session_relation_graph(&store.project_id)
        .await
        .expect_err("an outstanding client refuses the Store close");
    assert!(refusal.to_string().contains("ClientLeases"), "{refusal}");
    assert_eq!(
        store
            .registry
            .registered_schema_convergence_schedule_count_for_test(),
        2,
        "the rolled-back retirement restarts the interrupted plan"
    );
    assert!(
        client
            .workflow_storage()
            .await
            .err()
            .expect("nothing committed yet")
            .is_workflow_authority_recovering()
    );

    store.gate.release();
    assert_eq!(
        wait_for_schema_convergence_past_obstruction(&store.registry, &store.shard_id).await,
        RegisteredSchemaConvergenceStatus::Complete
    );
    client
        .workflow_storage()
        .await
        .expect("the same mount finishes the upgrade");
    let (digest, rows) = published_workflow_state(&store.sessions_path);
    assert_ne!(digest, PUBLISHED_WORKFLOW_DIGEST);
    assert_eq!(rows, store.published.1);
}

/// A store that stops being the proven shape refuses the upgrade for good. That
/// is the typed reset, never another attempt, however soon the timer would fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_upgrade_is_terminal_rather_than_retrying() {
    let (_temporary, identity, project_id, project_root, sessions_path, _database_scope) =
        project_sessions_convergence_fixture("project.published-workflow-refused", false).await;
    age_session_store_to_published_workflow_shape(&sessions_path);
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("session runtime registry");
    let gate = registry.block_registered_schema_convergence_for_test();
    let database = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("a published store admits");
    gate.wait_until_blocked().await;
    rusqlite::Connection::open(&sessions_path)
        .expect("open the session store")
        .execute_batch(
            "CREATE INDEX artifact_digest_copy ON workflow_artifact_payloads (byte_length)",
        )
        .expect("add an object the proven shape does not include");
    gate.retry_on_timer(std::time::Duration::from_millis(1));
    gate.release();

    let status = wait_for_schema_convergence_past_obstruction(&registry, &shard_id).await;
    assert!(
        matches!(status, RegisteredSchemaConvergenceStatus::Degraded { .. }),
        "a refusal must not read as retrying: {status:?}"
    );
    let attempts = registry.registered_schema_convergence_execution_count_for_test();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        registry.registered_schema_convergence_execution_count_for_test(),
        attempts,
        "a refusal is not attempted again"
    );
    assert!(
        !registry.reset_required_stores().is_empty(),
        "a refusal is recorded as the typed reset"
    );
    let evidence = schema_convergence_doctor_evidence(&registry);
    assert!(evidence.contains(".stopped."), "{evidence}");
    assert!(
        database.workflow_storage().await.is_err(),
        "a refused store does not serve workflow storage"
    );
}

/// Shutdown stops the upgrade before it commits, and a daemon restart over the
/// same store recognizes it and finishes the move.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_published_workflow_recovery_resumes_on_restart() {
    let (_temporary, identity, project_id, project_root, sessions_path, _database_scope) =
        project_sessions_convergence_fixture("project.published-workflow-restart", false).await;
    age_session_store_to_published_workflow_shape(&sessions_path);
    let published = published_workflow_state(&sessions_path);
    let shard_id = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    {
        let registry =
            DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity.clone(), true)
                .await
                .expect("session runtime registry");
        let convergence_gate = registry.block_registered_schema_convergence_for_test();
        let database = registry
            .project_sessions(project_id.clone(), [project_root.clone()])
            .await
            .expect("a published store admits");
        convergence_gate.wait_until_blocked().await;

        registry.cancel_terminal_tasks();
        tokio::time::timeout(
            MUST_NOT_BLOCK,
            registry
                .shutdown_terminal_tasks(&tracedecay_runtime_core::native_runtime::cx().unwrap()),
        )
        .await
        .expect("shutdown must not wait for the upgrade")
        .expect("session runtime terminal tasks shut down cleanly");
        assert!(
            database
                .workflow_storage()
                .await
                .err()
                .expect("nothing committed")
                .is_workflow_authority_recovering()
        );
    }
    assert_eq!(published_workflow_state(&sessions_path), published);

    let registry = DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(identity, true)
        .await
        .expect("restarted session runtime registry");
    let database = registry
        .project_sessions(project_id, [project_root])
        .await
        .expect("the restarted daemon admits the store");
    assert_eq!(
        wait_for_schema_convergence(&registry, &shard_id).await,
        RegisteredSchemaConvergenceStatus::Complete
    );
    database
        .workflow_storage()
        .await
        .expect("the restart finished the upgrade");
    assert_eq!(published_workflow_state(&sessions_path).1, published.1);
}

/// Runs background schema convergence on a fresh project-sessions shard and
/// returns its durable authority checkpoint's bounded-pass count.
async fn converged_authority_checkpoint_passes(project_name: &str, pending: bool) -> i64 {
    let (_temporary, identity, project_id, project_root, _sessions_path, _database_scope) =
        project_sessions_convergence_fixture(project_name, pending).await;
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
    rows.next()
        .await
        .expect("read checkpoint")
        .expect("durable checkpoint row")
        .get::<i64>(0)
        .expect("decode checkpoint")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_convergence_commits_the_durable_authority_checkpoint() {
    assert_eq!(
        converged_authority_checkpoint_passes("project.schema-checkpoint", true).await,
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_reopen_resumes_from_the_trusted_authority_checkpoint() {
    assert_eq!(
        converged_authority_checkpoint_passes("project.schema-trusted-checkpoint", false).await,
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
    let main_path = project_store_root.join(tracedecay_runtime_core::config::DB_FILENAME);
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
    match write_error {
        TraceDecayError::Database { operation, message } => {
            assert_eq!(operation, "write through read-only project graph facade");
            assert_eq!(
                message,
                format!(
                    "cannot upgrade the daemon's shared read-only connection at '{}' to writable; acquire writable ownership before opening read handles",
                    main_path.display()
                )
            );
        }
        other => panic!("read-only write denial must be a database error: {other}"),
    }

    let unpublished_error = match registry
        .project_graph_registered(
            project_id,
            unpublished_path.clone(),
            DatabaseAccessMode::ReadOnly,
        )
        .await
    {
        Ok(_) => panic!("unpublished project store inherited synthetic write authority"),
        Err(error) => error,
    };
    match unpublished_error {
        TraceDecayError::Database { operation, message } => {
            assert_eq!(operation, "reuse project graph runtime");
            assert_eq!(
                message,
                format!(
                    "project graph locator {} differs from retained canonical locator {}",
                    unpublished_path.display(),
                    main.canonical_database_path().display()
                )
            );
        }
        other => panic!("unpublished project store denial must be a database error: {other}"),
    }
    drop(unpublished_authority);
}

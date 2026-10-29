use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, atomic::AtomicBool};

use tracedecay_contracts::RequestId;
use tracedecay_contracts::remote::capture::RemoteWriterAuthorityV1;
use tracedecay_contracts::remote::protocol::RemoteProtocolRequestV1;
use tracedecay_contracts::remote::recovery::{
    PromotionConfirmationV1, RemoteRecoveryCallerV1, RemoteRecoveryControlPortV1,
    RemoteRecoveryInterruptionV1, RemoteRecoveryOperationErrorV1, RemoteRecoveryOperationPortV1,
};
use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{BrainNodeId, CurrentRemoteAuthorityStateV1, EntityId, UtcMicros};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationId, GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace,
    GraphProjectionId, GraphProjectionIdentity, GraphWatermark, SourceGeneration,
};
use tracedecay_runtime_core::db::{DatabaseAccessMode, DatabaseAuthority};
use tracedecay_runtime_core::shard_runtime::registry::StoreRuntimeRegistryFailure;
use tracedecay_rusqlite_runtime::remote::RemoteWriterPublicationErrorV1;
use tracedecay_store::{FactReadControl, ProjectId, RetainedGraphStoreLeaseV1, StoreShardIdV1};

use super::remote_mount_tests::{admission, grant, query_policy, replay_policy};
use super::{
    DaemonSessionRuntimeRegistryV1, RemoteRecoveryAdmission, RemoteRecoveryProjectLifecycle,
    process_runtime_generation, registry_open_error,
};

/// Bound for the assertions in this file that read "must not wait for
/// background recovery" or "must settle".
///
/// The regression each one catches is an operation that never returns, and no
/// finite bound reaches that, so the value only has to exceed the honest
/// latency of the operation itself. At two seconds it did not: a machine
/// running the whole lib suite across its cores opens a session database
/// slower than that, and the bound reported a loaded host as a blocked one.
const MUST_NOT_BLOCK: std::time::Duration = std::time::Duration::from_mins(1);

struct AdmitRemoteRecovery;

impl RemoteRecoveryProjectLifecycle for AdmitRemoteRecovery {
    fn authorize_project_recovery<'a>(
        &'a self,
        _project_id: &'a ProjectId,
    ) -> Pin<
        Box<
            dyn Future<Output = tracedecay_domain::errors::Result<RemoteRecoveryAdmission>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async { Ok(RemoteRecoveryAdmission::hold(())) })
    }
}

struct PromotionControl(Option<RemoteRecoveryInterruptionV1>);

impl RemoteRecoveryControlPortV1 for PromotionControl {
    fn interruption(&self, _request_id: &RequestId) -> Option<RemoteRecoveryInterruptionV1> {
        self.0
    }
}

struct ProvisionedWriterFixture {
    caller: RemoteRecoveryCallerV1,
    authority_key: tracedecay_domain::ManifestDigest,
    recovery: Arc<tracedecay_rusqlite_runtime::remote::RemoteRecoverySqliteAuthorityV1>,
    writer: RemoteWriterAuthorityV1,
    project_sessions: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    node_id: BrainNodeId,
    registry: DaemonSessionRuntimeRegistryV1,
    identity: LocalProfileIdentityAuthorityV1,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _daemon_authority: tracedecay_daemon_identity::authority::DaemonAuthority,
    _temporary: tempfile::TempDir,
}

impl ProvisionedWriterFixture {
    async fn publish(&self) -> Result<(), RemoteWriterPublicationErrorV1> {
        self.registry
            .publish_remote_writer_authority(
                &self.node_id,
                self.writer.clone(),
                replay_policy(&self.writer.scope),
                query_policy(&self.writer.scope),
            )
            .await
    }

    fn promotion(&self, request_id: &str) -> RemoteProtocolRequestV1<PromotionConfirmationV1> {
        RemoteProtocolRequestV1::new(
            RequestId::new(request_id).expect("request identity"),
            self.identity.brain_id().clone(),
            self.node_id.clone(),
            1,
            Some(self.writer.authority.fence.clone()),
            UtcMicros(2),
            PromotionConfirmationV1 {
                preview_id: format!("promotion.{request_id}"),
                expected_authority_epoch: 1,
                expected_placement_revision: 1,
                expires_at_micros: i64::MAX,
            },
        )
        .expect("promotion request")
    }
}

/// Mounts one project and provisions one `RemoteNode` for its writer scope,
/// without publishing the writer.
async fn provisioned_writer_fixture() -> ProvisionedWriterFixture {
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
        "remote failover classification",
    )
    .expect("daemon database scope");
    let identity = daemon_authority.profile_identity().clone();
    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("session runtime registry");
    registry
        .install_remote_recovery_project_lifecycle(Arc::new(AdmitRemoteRecovery))
        .expect("install recovery project lifecycle");
    let node_id = BrainNodeId::new("node.remote.cancel-test").expect("remote node identity");
    let project_id = ProjectId::new("project.cancel-test").expect("project identity");
    let project_root = temporary.path().join("cancel-test");
    std::fs::create_dir_all(&project_root).expect("project root");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let project_sessions = registry
        .project_sessions(project_id.clone(), [project_root])
        .await
        .expect("project sessions mount registers the replay target");

    let writer: RemoteWriterAuthorityV1 = serde_json::from_value(serde_json::json!({
        "project_id": project_id.as_str(),
        "scope": {
            "project_id": project_id.as_str(),
            "repository_id": "repository.cancel-test",
            "worktree_id": "worktree.cancel-test",
            "reference": "refs/heads/main",
            "snapshot_id": "snapshot.cancel-test"
        },
        "authority": {
            "fence": {
                "brain_id": identity.brain_id().as_str(),
                "shard_id": "shard.cancel-test",
                "generation_id": "generation.cancel-test",
                "placement_revision": 1,
                "authority_epoch": 1,
                "authority_node_id": "node.remote.previous-writer"
            },
            "credential_revision": 1,
            "observed_at": 1
        }
    }))
    .expect("remote writer authority");
    let mut grant = grant(identity.brain_id().clone(), node_id.clone(), &[5_u8; 32]);
    grant.scope = writer.scope.clone();
    registry
        .provision_remote_node(grant.clone(), admission(&grant))
        .await
        .expect("provision RemoteNode");
    let recovery = registry
        .remote_recovery_authority(&node_id)
        .await
        .expect("mounted remote recovery authority");
    let authority_key = writer
        .authority
        .fence
        .recovery_authority_key()
        .expect("recovery authority key");
    let caller = RemoteRecoveryCallerV1 {
        node_id: node_id.clone(),
        enrollment_id: EntityId::new("enrollment.cancel-test").expect("enrollment identity"),
        enrollment_revision: 1,
        scope: writer.scope.clone(),
    };
    ProvisionedWriterFixture {
        caller,
        authority_key,
        recovery,
        writer,
        project_sessions,
        node_id,
        registry,
        identity,
        _database_scope,
        _daemon_authority: daemon_authority,
        _temporary: temporary,
    }
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
        tracedecay_runtime_core::shard_runtime::registry::StoreRuntimeLookup::WrongIncarnation {
            expected,
            actual,
        } if *expected == stale && actual.as_ref() == &current
    ));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_failover_reports_corrupt_fence_text_naming_cancel_as_corruption_not_cancellation() {
    let fixture = provisioned_writer_fixture().await;
    let corrupt = fixture.promotion("request.cancel-test.corrupt");
    let interrupted = fixture.promotion("request.cancel-test.interrupted");
    fixture
        .publish()
        .await
        .expect("publish the current writer at every sink");

    // The fence row is valid JSON of the wrong shape; its decode error quotes
    // the text "cancel-test".
    fixture
        .project_sessions
        .writer_connection()
        .expect("project sessions writer")
        .execute_batch(&format!(
            "UPDATE remote_writer_fences SET writer_fence_json = '\"cancel-test\"'
             WHERE authority_key = '{}';",
            fixture.authority_key.as_str()
        ))
        .await
        .expect("corrupt the published writer fence");
    let promote_recovery = Arc::clone(&fixture.recovery);
    let promote_caller = fixture.caller.clone();
    let corrupt_outcome = tracedecay_runtime_core::native_runtime::on_blocking_pool(async move {
        promote_recovery
            .promote(&corrupt, &promote_caller, Arc::new(PromotionControl(None)))
            .await
            .err()
    })
    .await
    .expect("corrupt failover thread");

    fixture
        .project_sessions
        .writer_connection()
        .expect("project sessions writer")
        .execute_batch(&format!(
            "UPDATE remote_writer_fences SET writer_fence_json = '{}'
             WHERE authority_key = '{}';",
            serde_json::to_string(&fixture.writer.authority.fence).expect("encode writer fence"),
            fixture.authority_key.as_str()
        ))
        .await
        .expect("repair writer fence");
    let promote_recovery = Arc::clone(&fixture.recovery);
    let promote_caller = fixture.caller.clone();
    let interrupted_outcome =
        tracedecay_runtime_core::native_runtime::on_blocking_pool(async move {
            promote_recovery
                .promote(
                    &interrupted,
                    &promote_caller,
                    Arc::new(PromotionControl(Some(
                        RemoteRecoveryInterruptionV1::Cancelled,
                    ))),
                )
                .await
                .err()
        })
        .await
        .expect("cancelled failover thread");

    assert_eq!(
        (corrupt_outcome, interrupted_outcome),
        (
            Some(RemoteRecoveryOperationErrorV1::Corruption),
            Some(RemoteRecoveryOperationErrorV1::Cancelled),
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn republishing_a_writer_restores_a_lost_project_writer_fence_before_failover() {
    let fixture = provisioned_writer_fixture().await;
    let promotion = fixture.promotion("request.cancel-test.restored-fence");
    fixture.publish().await.expect("publish the first writer");
    fixture
        .project_sessions
        .writer_connection()
        .expect("project sessions writer")
        .execute_batch("DELETE FROM remote_writer_fences;")
        .await
        .expect("lose the ProjectSessions writer fence");

    let republished = fixture.publish().await;
    let promote_recovery = Arc::clone(&fixture.recovery);
    let promote_caller = fixture.caller.clone();
    let failover = tracedecay_runtime_core::native_runtime::on_blocking_pool(async move {
        promote_recovery
            .promote(
                &promotion,
                &promote_caller,
                Arc::new(PromotionControl(None)),
            )
            .await
            .map(|committed| {
                (
                    committed.output.previous_epoch,
                    committed.output.installed_epoch,
                    committed.output.old_authority_fenced,
                )
            })
    })
    .await
    .expect("failover thread");

    assert_eq!((republished, failover), (Ok(()), Ok((1, 2, true))));
}

/// The remote HTTP routes run writer publication and failover on the native
/// blocking pool, so neither may reach Tokio from there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_writer_publication_and_failover_settle_without_an_entered_tokio_runtime() {
    let fixture = provisioned_writer_fixture().await;
    let promotion = fixture.promotion("request.cancel-test.native-failover");
    let outcome = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tracedecay_runtime_core::native_runtime::block_on(async {
                    assert!(tokio::runtime::Handle::try_current().is_err());
                    let published = fixture.publish().await;
                    let promote_recovery = Arc::clone(&fixture.recovery);
                    let promote_caller = fixture.caller.clone();
                    let failover =
                        tracedecay_runtime_core::native_runtime::on_blocking_pool(async move {
                            assert!(tokio::runtime::Handle::try_current().is_err());
                            promote_recovery
                                .promote(
                                    &promotion,
                                    &promote_caller,
                                    Arc::new(PromotionControl(None)),
                                )
                                .await
                                .map(|committed| {
                                    (
                                        committed.output.previous_epoch,
                                        committed.output.installed_epoch,
                                    )
                                })
                        })
                        .await
                        .expect("native failover thread");
                    (published, failover)
                })
                .expect("native runtime")
            })
            .join()
            .expect("native caller thread")
    });

    assert_eq!(outcome, (Ok(()), Ok((1, 2))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failover_names_the_promoted_node_credential_as_the_authority_credential() {
    let fixture = provisioned_writer_fixture().await;
    let promotion = fixture.promotion("request.cancel-test.promoted-credential");
    fixture.publish().await.expect("publish the first writer");
    let mut caller = fixture.caller.clone();
    caller.enrollment_revision = fixture.writer.authority.credential_revision + 1;
    let expected = (fixture.node_id.clone(), caller.enrollment_revision);

    let promote_recovery = Arc::clone(&fixture.recovery);
    let authority = tracedecay_runtime_core::native_runtime::on_blocking_pool(async move {
        promote_recovery
            .promote(&promotion, &caller, Arc::new(PromotionControl(None)))
            .await
            .map(|committed| committed.authority)
    })
    .await
    .expect("failover thread")
    .expect("failover commits");

    let CurrentRemoteAuthorityStateV1::Available(authority) = authority else {
        panic!("failover reports an available authority, got {authority:?}");
    };
    assert_eq!(
        (
            authority.fence.authority_node_id,
            authority.credential_revision
        ),
        expected,
        "replay authenticates the promoted node's own enrollment revision"
    );
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

    let publication_operation = tokio::time::timeout(MUST_NOT_BLOCK, async {
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
    let database_path = project_store_root.join(tracedecay_runtime_core::config::DB_FILENAME);
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
        .shutdown_terminal_tasks(&tracedecay_runtime_core::native_runtime::cx().unwrap())
        .await
        .expect("first terminal tasks shut down");
    first_registry.cancel_memory_graph_reconciliation_tasks();
    first_registry
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("first reconciliation shuts down");
    first_registry
        .close_retained_graph_runtimes_for_shutdown(
            std::time::Instant::now() + std::time::Duration::from_mins(1),
        )
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
        MUST_NOT_BLOCK,
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
    tokio::time::timeout(MUST_NOT_BLOCK, async {
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
        MUST_NOT_BLOCK,
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
        .shutdown_terminal_tasks(&tracedecay_runtime_core::native_runtime::cx().unwrap())
        .await
        .expect("first terminal tasks shut down");
    first_registry
        .close_retained_graph_runtimes_for_shutdown(
            std::time::Instant::now() + std::time::Duration::from_mins(1),
        )
        .await
        .expect("first session relation graph closes");
    drop(first_registry);

    std::fs::write(&graph_path, b"corrupt session relation graph")
        .expect("corrupt session relation graph file");

    let reopened_registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("reopened session runtime registry");
    let reopened = tokio::time::timeout(
        MUST_NOT_BLOCK,
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
        .shutdown_terminal_tasks(&tracedecay_runtime_core::native_runtime::cx().unwrap())
        .await
        .expect("detached session relation graph task shuts down");
    reopened_registry
        .close_retained_graph_runtimes_for_shutdown(
            std::time::Instant::now() + std::time::Duration::from_mins(1),
        )
        .await
        .expect("detached relational session owner shuts down cleanly");
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

    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("read-only mount of a deleted DB must refuse"),
    };
    match error {
        TraceDecayError::Database { operation, .. } => {
            assert_eq!(operation, "mount project graph store read-only");
        }
        other => panic!("read-only mount of a deleted DB must refuse as a database error: {other}"),
    }
    assert!(
        !database_path.exists(),
        "read-only mount recreated the deleted worktree DB"
    );
}

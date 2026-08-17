use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tempfile::TempDir;
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationId, GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace,
    GraphProjectionId, GraphProjectionIdentity, GraphWatermark, SourceGeneration,
    VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::errors::TraceDecayError;
use tracedecay_store::{
    FactReadControl, GraphGenerationIdV1, GraphNamespaceV1, GraphProjectionIdV1,
    GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1, GraphPublicationKeyV1,
    GraphPublicationOperationContextV1, GraphPublicationStoreV1, GraphReplayAppendOutcomeV1,
    ProjectId, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1,
    RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    StoreShardIdV1, StoreShardScopeV1,
};

use super::DaemonSessionRuntimeRegistryV1;
use super::code_graph::inline_graph_publication_input_digest;
use crate::daemon::profile_identity;
use crate::global_db::{RegisteredGlobalDbLeaseV1, VerifiedGraphRuntimePortV1};

mod concurrency;
mod mount_scope;

struct ContractFixture {
    registry: DaemonSessionRuntimeRegistryV1,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    root: PathBuf,
    _temp: TempDir,
}

impl ContractFixture {
    async fn new(label: &str) -> Self {
        let temp = TempDir::new().expect("contract fixture root");
        let profile_root = temp.path().join("profile");
        let identity =
            profile_identity::load_or_create(&profile_root).expect("profile identity authority");
        let database_scope = crate::db::enter_daemon_database_scope(&profile_root, 29, label)
            .expect("daemon database scope");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("daemon session runtime registry");
        Self {
            registry,
            _database_scope: database_scope,
            root: temp.path().to_path_buf(),
            _temp: temp,
        }
    }

    fn project_roots(&self, project_id: &ProjectId) -> Vec<PathBuf> {
        vec![
            self.root.join(format!("{}-primary", project_id.as_str())),
            self.root.join(format!("{}-linked", project_id.as_str())),
        ]
    }

    async fn mount_unbound(
        &self,
        project_id: &ProjectId,
    ) -> (Arc<crate::db::Database>, RegisteredGlobalDbLeaseV1) {
        let roots = self.project_roots(project_id);
        for root in &roots {
            std::fs::create_dir_all(root).expect("worktree root");
            crate::storage::write_enrollment_marker(
                root,
                &crate::storage::EnrollmentMarker {
                    project_id: project_id.as_str().to_owned(),
                    storage_mode: crate::storage::StorageMode::ProfileSharded,
                },
            )
            .expect("project enrollment");
        }
        let project_database = self
            .registry
            .project_memory(project_id.clone(), roots.clone())
            .await
            .expect("project graph database");
        let sessions = self
            .registry
            .project_sessions(project_id.clone(), roots)
            .await
            .expect("project sessions database");
        (project_database, sessions)
    }

    async fn bind(
        &self,
        project_id: &ProjectId,
    ) -> (Arc<crate::db::Database>, RegisteredGlobalDbLeaseV1) {
        let (project_database, sessions) = self.mount_unbound(project_id).await;
        let runtime = project_database
            .memory_graph_runtime()
            .expect("database-issued weak graph proxy");
        assert!(
            sessions.bind_project_graph_runtime(runtime).is_ok(),
            "bind project graph proxy once"
        );
        (project_database, sessions)
    }
}

fn project_id(label: &str) -> ProjectId {
    ProjectId::new(format!("project.graph-port-contract.{label}")).expect("project id")
}

fn projection(label: &str) -> GraphProjectionIdentity {
    GraphProjectionIdentity::new(
        GraphNamespace::new("project").expect("graph namespace"),
        GraphProjectionId::new(format!("projection.{label}")).expect("projection id"),
    )
}

fn manifest(
    projection: &GraphProjectionIdentity,
    generation: &str,
    watermark: &str,
) -> GraphGenerationManifest {
    GraphGenerationManifest::new(
        projection.clone(),
        GraphGenerationId::new(format!("generation.{generation}")).expect("generation id"),
        SourceGeneration::new(format!("source.{generation}")).expect("source generation"),
        GraphWatermark::new(format!("watermark.{watermark}")).expect("graph watermark"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("graph generation manifest")
}

fn key(label: &str) -> GraphIdempotencyKey {
    GraphIdempotencyKey::new(format!("publication.{label}")).expect("idempotency key")
}

fn cancellation(cancelled: bool) -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(cancelled))
}

fn publish_through_trait(
    port: &dyn VerifiedGraphRuntimePortV1,
    manifest: &GraphGenerationManifest,
    idempotency_key: GraphIdempotencyKey,
    cancelled: bool,
) -> Result<VerifiedGraphSnapshot, GraphDbError> {
    port.publish_verified_manifest(manifest, idempotency_key, cancellation(cancelled))
}

fn reconcile_through_trait(
    port: &dyn VerifiedGraphRuntimePortV1,
    manifest: &GraphGenerationManifest,
    idempotency_key: GraphIdempotencyKey,
) -> Result<VerifiedGraphSnapshot, GraphDbError> {
    port.reconcile_verified_manifest(manifest, idempotency_key)
}

fn snapshot_through_trait(
    port: &dyn VerifiedGraphRuntimePortV1,
    projection: &GraphProjectionIdentity,
) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
    port.verified_snapshot(projection, FactReadControl::new(Arc::new(|| false)))
}

fn mounted_graph_operation(
    database: &crate::db::Database,
) -> crate::db::MemoryGraphRuntimeOperationV1 {
    database
        .issue_memory_graph_runtime_operation()
        .expect("mounted database issues one graph operation")
}

fn publish_through_database(
    database: &crate::db::Database,
    manifest: &GraphGenerationManifest,
    idempotency_key: GraphIdempotencyKey,
    cancelled: bool,
) -> Result<VerifiedGraphSnapshot, GraphDbError> {
    let operation = mounted_graph_operation(database);
    publish_through_trait(operation.runtime(), manifest, idempotency_key, cancelled)
}

fn reconcile_through_database(
    database: &crate::db::Database,
    manifest: &GraphGenerationManifest,
    idempotency_key: GraphIdempotencyKey,
) -> Result<VerifiedGraphSnapshot, GraphDbError> {
    let operation = mounted_graph_operation(database);
    reconcile_through_trait(operation.runtime(), manifest, idempotency_key)
}

fn snapshot_through_database(
    database: &crate::db::Database,
    projection: &GraphProjectionIdentity,
) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
    let operation = mounted_graph_operation(database);
    snapshot_through_trait(operation.runtime(), projection)
}

#[tokio::test]
async fn runtime_binding_is_absent_before_bind_and_rejects_double_bind() {
    let fixture = ContractFixture::new("binding").await;
    let project_id = project_id("binding");
    let (project_database, sessions) = fixture.mount_unbound(&project_id).await;
    assert!(sessions.project_graph_runtime().is_none());

    let runtime = project_database
        .memory_graph_runtime()
        .expect("database-issued weak graph proxy");
    assert!(
        sessions.bind_project_graph_runtime(runtime.clone()).is_ok(),
        "first graph proxy binding"
    );
    let rejected = sessions
        .bind_project_graph_runtime(runtime.clone())
        .expect_err("second graph proxy binding must be rejected");

    let first = mounted_graph_operation(&project_database);
    let second = mounted_graph_operation(&project_database);
    for operation in [&first, &second] {
        assert_eq!(
            operation.runtime().relational_binding(),
            project_database.registered_binding()
        );
        assert_eq!(
            operation.runtime().relational_verified_locator(),
            project_database.registered_verified_locator()
        );
    }
    let bound = sessions
        .project_graph_runtime()
        .expect("bound graph proxy remains available");
    assert_eq!(bound.relational_binding(), runtime.relational_binding());
    assert_eq!(
        bound.relational_verified_locator(),
        runtime.relational_verified_locator()
    );
    assert_eq!(rejected.relational_binding(), runtime.relational_binding());
    assert_eq!(
        rejected.relational_verified_locator(),
        runtime.relational_verified_locator()
    );
}

#[tokio::test]
async fn memory_graph_operations_remain_isolated_by_exact_relational_identity() {
    let fixture = ContractFixture::new("memory-binding").await;
    let first_id = project_id("memory-binding-first");
    let second_id = project_id("memory-binding-second");
    let (first_database, first_sessions) = fixture.mount_unbound(&first_id).await;
    let (second_database, _) = fixture.mount_unbound(&second_id).await;
    let first = mounted_graph_operation(&first_database);
    let second = mounted_graph_operation(&second_database);
    assert_ne!(
        first.runtime().relational_binding(),
        second.runtime().relational_binding()
    );
    assert_ne!(
        first.runtime().relational_verified_locator(),
        second.runtime().relational_verified_locator()
    );
    let second_proxy = second_database
        .memory_graph_runtime()
        .expect("second project graph proxy");
    assert!(
        first_sessions
            .bind_project_graph_runtime(second_proxy)
            .is_err(),
        "a second project's proxy cannot bind to this session database"
    );

    let profile_database = fixture
        .registry
        .profile_memory()
        .await
        .expect("profile memory database");
    let profile = mounted_graph_operation(&profile_database);
    assert_ne!(
        first.runtime().relational_binding(),
        profile.runtime().relational_binding()
    );
    assert_ne!(
        first.runtime().relational_verified_locator(),
        profile.runtime().relational_verified_locator()
    );
    let profile_proxy = profile_database
        .memory_graph_runtime()
        .expect("profile graph proxy");
    assert!(
        first_sessions
            .bind_project_graph_runtime(profile_proxy)
            .is_err(),
        "a profile proxy cannot bind to a project session database"
    );

    let own_proxy = first_database
        .memory_graph_runtime()
        .expect("first project graph proxy");
    assert!(first_sessions.bind_project_graph_runtime(own_proxy).is_ok());
}

#[tokio::test]
async fn read_only_memory_database_rejects_a_writer_graph_runtime() {
    let fixture = ContractFixture::new("read-only-binding").await;
    let project_id = project_id("read-only-binding");
    let (database, _sessions) = fixture.bind(&project_id).await;
    let projection = projection("read-only-binding");
    let manifest = manifest(&projection, "read-only-binding", "1");
    let initial = publish_through_database(&database, &manifest, key("read-only-binding"), false)
        .expect("initial writer publication");
    let read_only = fixture
        .registry
        .project_memory_read_only(project_id.clone(), fixture.project_roots(&project_id))
        .await
        .expect("read-only project memory database");

    assert!(!read_only.is_writable());
    assert!(matches!(
        read_only.issue_memory_graph_runtime_operation(),
        Err(crate::db::MemoryGraphRuntimeOperationErrorV1::Unbound)
    ));
    let retained = snapshot_through_database(&database, &projection)
        .expect("verified snapshot after rejected read-only bind")
        .expect("initial verified head");
    assert_eq!(retained.verified_head(), initial.verified_head());
}

#[tokio::test]
async fn bound_verified_port_does_not_retain_the_database_facade() {
    let fixture = ContractFixture::new("no-database-cycle").await;
    let project_id = project_id("no-database-cycle");
    let (database, _sessions) = fixture.mount_unbound(&project_id).await;
    let weak_database = Arc::downgrade(&database);
    {
        let operation = mounted_graph_operation(&database);
        assert!(matches!(
            &operation.runtime().relational_binding().shard_id.scope,
            StoreShardScopeV1::Project {
                project_id: bound_project,
            } if bound_project == &project_id
        ));
    }
    database
        .memory_graph_reconciliation_task_owner()
        .expect("bound graph runtime task owner")
        .shutdown()
        .await
        .expect("join initial reconciliation task");

    drop((database, fixture));

    assert!(weak_database.upgrade().is_none());
}

#[tokio::test]
async fn publish_denies_pre_cancel_without_consuming_the_publication() {
    let fixture = ContractFixture::new("pre-cancel").await;
    let project_id = project_id("pre-cancel");
    let (database, _sessions) = fixture.mount_unbound(&project_id).await;
    let projection = projection("pre-cancel");
    let manifest = manifest(&projection, "pre-cancel", "1");
    assert!(matches!(
        publish_through_database(&database, &manifest, key("pre-cancel"), true),
        Err(GraphDbError::Cancelled)
    ));
    let published = publish_through_database(&database, &manifest, key("pre-cancel"), false)
        .expect("cancelled attempt must not consume publication");
    assert_eq!(published.generation(), &manifest.generation);
}

#[tokio::test]
async fn exact_publication_replay_returns_the_same_verified_head() {
    let fixture = ContractFixture::new("exact-replay").await;
    let project_id = project_id("exact-replay");
    let (database, _sessions) = fixture.mount_unbound(&project_id).await;
    let projection = projection("exact-replay");
    let initial_manifest = manifest(&projection, "exact-replay", "1");
    let first = reconcile_through_database(&database, &initial_manifest, key("exact-replay"))
        .expect("initial lifecycle reconciliation");
    let replay = reconcile_through_database(&database, &initial_manifest, key("exact-replay"))
        .expect("exact lifecycle reconciliation replay");
    let changed = manifest(&projection, "exact-replay", "changed");

    assert_eq!(replay.projection(), first.projection());
    assert_eq!(replay.generation(), first.generation());
    assert_eq!(replay.verified_head(), first.verified_head());
    assert!(matches!(
        reconcile_through_database(&database, &changed, key("exact-replay")),
        Err(GraphDbError::Conflict)
    ));
    let retained = snapshot_through_database(&database, &projection)
        .expect("verified snapshot after changed-input conflict")
        .expect("initial verified head remains available");
    assert_eq!(retained.verified_head(), first.verified_head());
}

#[tokio::test]
async fn stale_republication_conflicts_after_a_new_head_wins() {
    let fixture = ContractFixture::new("stale-republication").await;
    let project_id = project_id("stale-republication");
    let (database, _sessions) = fixture.mount_unbound(&project_id).await;
    let projection = projection("stale-republication");
    let first = manifest(&projection, "stale-first", "1");
    let second = manifest(&projection, "stale-second", "2");
    publish_through_database(&database, &first, key("stale-first"), false)
        .expect("first publication");
    publish_through_database(&database, &second, key("stale-second"), false)
        .expect("new head publication");

    assert!(matches!(
        publish_through_database(&database, &first, key("stale-first"), false),
        Err(GraphDbError::Conflict)
    ));
}

/// The first-ever publish of a fresh projection drives two irreversible
/// durable commits — the relational journal append and the verified-head
/// CAS — each of which must hold its own at-most-once commit grant. Routing
/// both through one arbitration context let the append consume the grant, so
/// the CAS was refused on every first publish and surfaced as infrastructure
/// unavailability ("relational graph publication authority is unavailable").
#[tokio::test]
async fn first_publish_of_a_fresh_projection_installs_the_verified_head() {
    let fixture = ContractFixture::new("first-publish").await;
    let project_id = project_id("first-publish");
    let (database, _sessions) = fixture.mount_unbound(&project_id).await;
    let projection = projection("first-publish");
    let manifest = manifest(&projection, "first-publish", "1");
    let published = publish_through_database(&database, &manifest, key("first-publish"), false)
        .expect("first publish must journal the replay and install the verified head");
    assert_eq!(published.generation(), &manifest.generation);

    let head = snapshot_through_database(&database, &projection)
        .expect("verified snapshot read after the first publish")
        .expect("verified head must be visible after the first publish");
    assert_eq!(head.generation(), &manifest.generation);
    assert_eq!(head.verified_head(), published.verified_head());
}

/// A projection that has never published a verified head is a typed empty
/// start (`Ok(None)`), not an unavailability error. Treating it as retryable
/// unavailability wedged fresh projects in an endless ingest retry loop.
#[tokio::test]
async fn never_published_projection_is_a_typed_empty_snapshot() {
    let fixture = ContractFixture::new("missing-projection").await;
    let project_id = project_id("missing-projection");
    let (database, _sessions) = fixture.mount_unbound(&project_id).await;

    assert!(matches!(
        snapshot_through_database(&database, &projection("never-published")),
        Ok(None)
    ));
}

#[tokio::test]
async fn project_graph_publications_are_isolated_by_project_shard() {
    let fixture = ContractFixture::new("project-isolation").await;
    let first_id = project_id("isolation-first");
    let second_id = project_id("isolation-second");
    let (first_database, _first_sessions) = fixture.mount_unbound(&first_id).await;
    let (second_database, _second_sessions) = fixture.mount_unbound(&second_id).await;
    let projection = projection("project-isolation");
    let manifest = manifest(&projection, "project-isolation", "1");

    publish_through_database(&first_database, &manifest, key("project-isolation"), false)
        .expect("first project publication");

    // The second shard never published this projection, so it must observe
    // the typed empty start — never the first shard's head.
    assert!(matches!(
        snapshot_through_database(&second_database, &projection),
        Ok(None)
    ));
}

#[tokio::test]
async fn exact_shard_retirement_leaves_sibling_live_and_remounts_fresh() {
    let fixture = ContractFixture::new("exact-shard-retirement").await;
    let first_id = project_id("retire-first");
    let second_id = project_id("retire-second");
    let (first_database, first_sessions) = fixture.mount_unbound(&first_id).await;
    let (second_database, _second_sessions) = fixture.mount_unbound(&second_id).await;
    let first_projection = projection("retire-first");
    let second_projection = projection("retire-second");
    let first_manifest = manifest(&first_projection, "retire-first", "1");
    let second_manifest = manifest(&second_projection, "retire-second", "1");
    let first_shard = first_database.registered_binding().shard_id.clone();

    fixture
        .registry
        .retire_memory_graph_reconciliation_task(&first_shard)
        .await
        .expect("retire exact first-shard reconciliation owner");
    assert!(matches!(
        reconcile_through_database(&first_database, &first_manifest, key("retire-first")),
        Err(GraphDbError::Cancelled)
    ));
    reconcile_through_database(&second_database, &second_manifest, key("retire-second"))
        .expect("sibling reconciliation remains live");

    fixture
        .registry
        .drop_project_runtime_caches(&first_id)
        .await;
    drop((first_database, first_sessions));
    let remounted = fixture
        .registry
        .project_memory(first_id.clone(), fixture.project_roots(&first_id))
        .await
        .expect("same project remounts with a fresh lifecycle");
    reconcile_through_database(&remounted, &first_manifest, key("retire-first-remounted"))
        .expect("remounted reconciliation lifecycle is fresh");
}

#[cfg(unix)]
#[tokio::test]
async fn exact_shard_retirement_closes_retained_graph_after_root_is_absent() {
    let fixture = ContractFixture::new("absent-root-retirement").await;
    let project_id = project_id("absent-root-retirement");
    let (database, _sessions) = fixture.mount_unbound(&project_id).await;
    let shard = database.registered_binding().shard_id.clone();
    let graph_manifest = manifest(
        &projection("absent-root-retirement"),
        "absent-root-retirement",
        "1",
    );

    std::fs::remove_dir_all(&fixture.root).expect("remove exact retained project store root");
    fixture
        .registry
        .retire_memory_graph_reconciliation_task(&shard)
        .await
        .expect("retained graph identity closes after its filesystem root is absent");

    assert!(matches!(
        reconcile_through_database(&database, &graph_manifest, key("absent-root-retirement")),
        Err(GraphDbError::Cancelled)
    ));
}

#[tokio::test]
async fn session_relation_close_refusal_restores_route_and_retry_closes_exact_graph() {
    let fixture = ContractFixture::new("session-relation-close-retry").await;
    let project_id = project_id("session-relation-close-retry");
    let (_project_database, external_old_sessions) = fixture.mount_unbound(&project_id).await;
    let old_binding = external_old_sessions.binding().clone();

    let refusal = fixture
        .registry
        .retire_project_session_relation_graph(&project_id)
        .await
        .expect_err("external old session facade must refuse graph close");
    match refusal {
        TraceDecayError::Database { operation, message } => {
            assert_eq!(operation, "close graph runtime");
            assert!(
                message.contains("graph database conflict"),
                "unexpected close refusal: {message}"
            );
        }
        other => panic!("unexpected close refusal: {other:?}"),
    }

    let restored = fixture
        .registry
        .mounted_project_sessions(&project_id)
        .await
        .expect("close refusal restores a mounted ProjectSessions route");
    assert!(
        !external_old_sessions.shares_client_with(&restored),
        "restoration must not reinsert the facade that still leases the old graph handle"
    );
    let (replay_binding, replay_path) = fixture
        .registry
        .remote_replay_transaction
        .target_descriptor(&project_id)
        .expect("close refusal restores the replay route");
    assert_eq!(replay_binding, *restored.binding());
    assert_eq!(replay_path, restored.db_path());
    assert_eq!(old_binding.shard_id, replay_binding.shard_id);

    drop((external_old_sessions, restored));
    fixture
        .registry
        .retire_project_session_relation_graph(&project_id)
        .await
        .expect("retry closes the exact graph after the external owner drops");
    assert!(
        fixture
            .registry
            .mounted_project_sessions(&project_id)
            .await
            .is_none(),
        "successful retry removes the ProjectSessions route"
    );
    assert!(
        fixture
            .registry
            .remote_replay_transaction
            .target_descriptor(&project_id)
            .is_err(),
        "successful retry removes the replay route"
    );
}

#[tokio::test]
async fn linked_worktree_roots_share_the_project_graph_runtime_authority() {
    let fixture = ContractFixture::new("linked-worktrees").await;
    let project_id = project_id("linked-worktrees");
    let (project_database, _sessions) = fixture.mount_unbound(&project_id).await;
    let projection = projection("linked-worktrees");
    let manifest = manifest(&projection, "linked-worktrees", "1");

    publish_through_database(&project_database, &manifest, key("linked-worktrees"), false)
        .expect("primary worktree publication");
    let linked_snapshot = snapshot_through_database(&project_database, &projection)
        .expect("linked worktree reads shared project graph")
        .expect("published verified head");

    assert_eq!(linked_snapshot.generation(), &manifest.generation);
}

#[tokio::test]
async fn project_and_profile_memory_verified_heads_survive_registry_restart() {
    let temporary = TempDir::new().expect("restart fixture root");
    let profile_root = temporary.path().join("profile");
    let project_id = project_id("verified-restart");
    let project_root = temporary.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    crate::storage::write_enrollment_marker(
        &project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("project enrollment");
    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let scope = crate::db::enter_daemon_database_scope(&profile_root, 31, "verified graph restart")
        .expect("first database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("first registry");
    let project_database = registry
        .project_memory(project_id.clone(), [project_root.clone()])
        .await
        .expect("project memory database");
    let project_projection = projection("verified-restart-project");
    let project_manifest = manifest(&project_projection, "verified-restart-project", "1");
    let project_snapshot = publish_through_database(
        &project_database,
        &project_manifest,
        key("verified-restart-project"),
        false,
    )
    .expect("project publication");
    let project_head = project_snapshot.verified_head().clone();
    drop(project_snapshot);

    let profile_database = registry
        .profile_memory()
        .await
        .expect("profile memory database");
    let profile_projection = projection("verified-restart-profile");
    let profile_manifest = manifest(&profile_projection, "verified-restart-profile", "1");
    let profile_snapshot = publish_through_database(
        &profile_database,
        &profile_manifest,
        key("verified-restart-profile"),
        false,
    )
    .expect("profile publication");
    let profile_head = profile_snapshot.verified_head().clone();
    drop(profile_snapshot);
    drop((project_database, profile_database, registry, scope));

    let _restarted_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 32, "verified graph restart reopen")
            .expect("restarted database scope");
    let restarted = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("restarted registry");
    let restarted_project_database = restarted
        .project_memory(project_id.clone(), [project_root])
        .await
        .expect("restarted project memory database");
    let recovered_project =
        snapshot_through_database(&restarted_project_database, &project_projection)
            .expect("recover project verified head")
            .expect("project verified head");
    assert_eq!(recovered_project.verified_head(), &project_head);

    let restarted_profile_database = restarted
        .profile_memory()
        .await
        .expect("restarted profile memory database");
    let recovered_profile =
        snapshot_through_database(&restarted_profile_database, &profile_projection)
            .expect("recover profile verified head")
            .expect("profile verified head");
    assert_eq!(recovered_profile.verified_head(), &profile_head);
}

struct NeverInterruptedProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl RuntimeRequestProbeV1 for NeverInterruptedProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }

    fn try_begin_commit(&self) -> bool {
        true
    }
}

/// A publish interrupted between the relational journal append and the
/// verified-head CAS leaves an active replay with no head. The next publish of
/// the same publication must resume it to a verified snapshot — answering
/// Conflict instead wedges the projection permanently (every later publish and
/// read fails until the store is deleted).
#[tokio::test]
async fn journaled_publication_without_a_head_resumes_to_a_verified_snapshot() {
    let fixture = ContractFixture::new("resume-journaled").await;
    let project_id = project_id("resume-journaled");
    let (project_database, _sessions) = fixture.mount_unbound(&project_id).await;
    let projection = projection("resume-journaled");
    let initial_manifest = manifest(&projection, "resume-journaled", "1");

    // Journal the replay exactly as the interrupted publish leaves it: the
    // append committed, the verified-head CAS never ran. The shard is built
    // the same way the retained runtime builds its authority binding so the
    // resumed publish resolves this exact journal row.
    let identity = profile_identity::load_or_create(&fixture.root.join("profile"))
        .expect("profile identity authority");
    let shard_id = StoreShardIdV1::project(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
    );
    let idempotency_key = key("resume-journaled");
    let publication_key = GraphPublicationKeyV1::new(
        GraphProjectionIdentityV1 {
            shard_id: shard_id.clone(),
            namespace: GraphNamespaceV1::new(initial_manifest.projection.namespace.as_str())
                .expect("relational namespace"),
            projection: GraphProjectionIdV1::new(initial_manifest.projection.projection.as_str())
                .expect("relational projection"),
        },
        GraphGenerationIdV1::new(initial_manifest.generation.as_str())
            .expect("relational generation"),
        GraphPublicationIdempotencyKeyV1::new(idempotency_key.as_str())
            .expect("relational idempotency key"),
    );
    let input_digest = inline_graph_publication_input_digest(&publication_key, &initial_manifest)
        .expect("canonical inline publication digest");
    let replay = initial_manifest
        .relational_replay(shard_id, idempotency_key, input_digest, None, &|| Ok(()))
        .expect("relational replay");
    let cancellation_identity = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new("resume-journaled-cancellation")
            .expect("cancellation id"),
        generation: 1,
    };
    let deadline_identity = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new("resume-journaled-deadline").expect("deadline id"),
    };
    let control = RuntimeRequestControlV1 {
        requested_at: UtcMicros(1),
        deadline: deadline_identity.clone(),
        cancellation: cancellation_identity.clone(),
    };
    let probe = NeverInterruptedProbe {
        cancellation: cancellation_identity,
        deadline: deadline_identity,
    };
    let context = GraphPublicationOperationContextV1::new(&control, &probe)
        .expect("publication operation context");
    let mut storage = project_database
        .graph_publication_storage()
        .expect("graph publication storage");
    assert!(matches!(
        storage
            .append_replay(&replay, &context)
            .expect("journal the replay"),
        GraphReplayAppendOutcomeV1::Appended(_)
    ));
    drop(storage);

    let changed = manifest(&projection, "resume-journaled", "changed");
    assert!(matches!(
        publish_through_database(&project_database, &changed, key("resume-journaled"), false),
        Err(GraphDbError::Conflict)
    ));
    let published = publish_through_database(
        &project_database,
        &initial_manifest,
        key("resume-journaled"),
        false,
    )
    .expect("journaled publication must resume to a verified snapshot");
    assert_eq!(published.generation(), &initial_manifest.generation);
    let snapshot = snapshot_through_database(&project_database, &projection)
        .expect("verified snapshot after the resume")
        .expect("published verified head");
    assert_eq!(snapshot.generation(), &initial_manifest.generation);
}

#[tokio::test]
async fn registry_drop_cancels_retained_trait_runtime_operations() {
    let fixture = ContractFixture::new("lifecycle-cancellation").await;
    let project_id = project_id("lifecycle-cancellation");
    let (database, _sessions) = fixture.mount_unbound(&project_id).await;
    let projection = projection("lifecycle-cancellation");
    let manifest = manifest(&projection, "lifecycle-cancellation", "1");
    let operation = mounted_graph_operation(&database);

    drop(fixture);

    assert!(matches!(
        publish_through_trait(
            operation.runtime(),
            &manifest,
            key("lifecycle-cancellation"),
            false,
        ),
        Err(GraphDbError::Cancelled)
    ));
    assert!(matches!(
        reconcile_through_trait(
            operation.runtime(),
            &manifest,
            key("lifecycle-cancellation"),
        ),
        Err(GraphDbError::Cancelled)
    ));
    assert!(matches!(
        snapshot_through_trait(operation.runtime(), &projection),
        Err(GraphDbError::Cancelled)
    ));
}

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
use tracedecay_store::{
    GraphPublicationInputDigestV1, GraphPublicationOperationContextV1, GraphPublicationStoreV1,
    GraphReplayAppendOutcomeV1, ProjectId, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestControlV1,
    RuntimeRequestProbeV1, StoreShardIdV1,
};

use super::DaemonSessionRuntimeRegistryV1;
use crate::daemon::profile_identity;
use crate::global_db::{ProjectGraphRuntimePortV1, RegisteredGlobalDb};

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

    async fn mount_unbound(
        &self,
        project_id: &ProjectId,
    ) -> (Arc<crate::db::Database>, Arc<RegisteredGlobalDb>) {
        let first_root = self.root.join(format!("{}-primary", project_id.as_str()));
        let linked_root = self.root.join(format!("{}-linked", project_id.as_str()));
        let roots = vec![first_root, linked_root];
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
    ) -> (
        Arc<crate::db::Database>,
        Arc<RegisteredGlobalDb>,
        Arc<dyn ProjectGraphRuntimePortV1>,
    ) {
        let (project_database, sessions) = self.mount_unbound(project_id).await;
        let runtime = self
            .registry
            .retain_project_graph_runtime(project_id.clone(), Arc::clone(&project_database))
            .await
            .expect("retained project graph runtime");
        let runtime: Arc<dyn ProjectGraphRuntimePortV1> = Arc::new(runtime);
        assert!(
            sessions
                .bind_project_graph_runtime(Arc::clone(&runtime))
                .is_ok(),
            "bind project graph runtime once"
        );
        (project_database, sessions, runtime)
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
    port: &dyn ProjectGraphRuntimePortV1,
    manifest: &GraphGenerationManifest,
    idempotency_key: GraphIdempotencyKey,
    cancelled: bool,
) -> Result<VerifiedGraphSnapshot, GraphDbError> {
    port.publish_verified_manifest(manifest, idempotency_key, cancellation(cancelled))
}

fn snapshot_through_trait(
    port: &dyn ProjectGraphRuntimePortV1,
    projection: &GraphProjectionIdentity,
) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
    port.verified_snapshot(projection, cancellation(false))
}

#[tokio::test]
async fn runtime_binding_is_absent_before_bind_and_rejects_double_bind() {
    let fixture = ContractFixture::new("binding").await;
    let project_id = project_id("binding");
    let (project_database, sessions) = fixture.mount_unbound(&project_id).await;
    assert!(sessions.project_graph_runtime().is_none());

    let runtime = fixture
        .registry
        .retain_project_graph_runtime(project_id, project_database)
        .await
        .expect("retained project graph runtime");
    let runtime: Arc<dyn ProjectGraphRuntimePortV1> = Arc::new(runtime);
    assert!(
        sessions
            .bind_project_graph_runtime(Arc::clone(&runtime))
            .is_ok(),
        "first runtime binding"
    );
    let rejected = sessions
        .bind_project_graph_runtime(Arc::clone(&runtime))
        .expect_err("second runtime binding must be rejected");

    assert!(Arc::ptr_eq(
        sessions.project_graph_runtime().expect("bound runtime"),
        &runtime
    ));
    assert!(Arc::ptr_eq(&rejected, &runtime));
}

#[tokio::test]
async fn publish_denies_pre_cancel_without_consuming_the_publication() {
    let fixture = ContractFixture::new("pre-cancel").await;
    let project_id = project_id("pre-cancel");
    let (_, sessions, _) = fixture.bind(&project_id).await;
    let projection = projection("pre-cancel");
    let manifest = manifest(&projection, "pre-cancel", "1");
    let port = sessions
        .project_graph_runtime()
        .expect("bound project graph runtime")
        .as_ref();

    assert!(matches!(
        publish_through_trait(port, &manifest, key("pre-cancel"), true),
        Err(GraphDbError::Cancelled)
    ));
    let published = publish_through_trait(port, &manifest, key("pre-cancel"), false)
        .expect("cancelled attempt must not consume publication");
    assert_eq!(published.generation(), &manifest.generation);
}

#[tokio::test]
async fn exact_publication_replay_returns_the_same_verified_head() {
    let fixture = ContractFixture::new("exact-replay").await;
    let project_id = project_id("exact-replay");
    let (_, sessions, _) = fixture.bind(&project_id).await;
    let projection = projection("exact-replay");
    let manifest = manifest(&projection, "exact-replay", "1");
    let port = sessions
        .project_graph_runtime()
        .expect("bound project graph runtime")
        .as_ref();

    let first = publish_through_trait(port, &manifest, key("exact-replay"), false)
        .expect("initial publication");
    let replay = publish_through_trait(port, &manifest, key("exact-replay"), false)
        .expect("exact publication replay");

    assert_eq!(replay.projection(), first.projection());
    assert_eq!(replay.generation(), first.generation());
    assert_eq!(replay.verified_head(), first.verified_head());
}

#[tokio::test]
async fn stale_republication_conflicts_after_a_new_head_wins() {
    let fixture = ContractFixture::new("stale-republication").await;
    let project_id = project_id("stale-republication");
    let (_, sessions, _) = fixture.bind(&project_id).await;
    let projection = projection("stale-republication");
    let first = manifest(&projection, "stale-first", "1");
    let second = manifest(&projection, "stale-second", "2");
    let port = sessions
        .project_graph_runtime()
        .expect("bound project graph runtime")
        .as_ref();

    publish_through_trait(port, &first, key("stale-first"), false).expect("first publication");
    publish_through_trait(port, &second, key("stale-second"), false).expect("new head publication");

    assert!(matches!(
        publish_through_trait(port, &first, key("stale-first"), false),
        Err(GraphDbError::Conflict)
    ));
}

/// A projection that has never published a verified head is a typed empty
/// start (`Ok(None)`), not an unavailability error. Treating it as retryable
/// unavailability wedged fresh projects in an endless ingest retry loop.
#[tokio::test]
async fn never_published_projection_is_a_typed_empty_snapshot() {
    let fixture = ContractFixture::new("missing-projection").await;
    let project_id = project_id("missing-projection");
    let (_, sessions, _) = fixture.bind(&project_id).await;
    let port = sessions
        .project_graph_runtime()
        .expect("bound project graph runtime")
        .as_ref();

    assert!(matches!(
        snapshot_through_trait(port, &projection("never-published")),
        Ok(None)
    ));
}

#[tokio::test]
async fn project_graph_publications_are_isolated_by_project_shard() {
    let fixture = ContractFixture::new("project-isolation").await;
    let first_id = project_id("isolation-first");
    let second_id = project_id("isolation-second");
    let (_, first_sessions, _) = fixture.bind(&first_id).await;
    let (_, second_sessions, _) = fixture.bind(&second_id).await;
    let projection = projection("project-isolation");
    let manifest = manifest(&projection, "project-isolation", "1");

    publish_through_trait(
        first_sessions
            .project_graph_runtime()
            .expect("first project runtime")
            .as_ref(),
        &manifest,
        key("project-isolation"),
        false,
    )
    .expect("first project publication");

    // The second shard never published this projection, so it must observe
    // the typed empty start — never the first shard's head.
    assert!(matches!(
        snapshot_through_trait(
            second_sessions
                .project_graph_runtime()
                .expect("second project runtime")
                .as_ref(),
            &projection,
        ),
        Ok(None)
    ));
}

#[tokio::test]
async fn linked_worktree_roots_share_the_project_graph_runtime_authority() {
    let fixture = ContractFixture::new("linked-worktrees").await;
    let project_id = project_id("linked-worktrees");
    let (project_database, sessions, _) = fixture.bind(&project_id).await;
    let linked_runtime = fixture
        .registry
        .retain_project_graph_runtime(project_id, project_database)
        .await
        .expect("linked worktree project graph runtime");
    let linked_runtime: Arc<dyn ProjectGraphRuntimePortV1> = Arc::new(linked_runtime);
    let projection = projection("linked-worktrees");
    let manifest = manifest(&projection, "linked-worktrees", "1");

    publish_through_trait(
        sessions
            .project_graph_runtime()
            .expect("primary worktree runtime")
            .as_ref(),
        &manifest,
        key("linked-worktrees"),
        false,
    )
    .expect("primary worktree publication");
    let linked_snapshot = snapshot_through_trait(linked_runtime.as_ref(), &projection)
        .expect("linked worktree reads shared project graph")
        .expect("published verified head");

    assert_eq!(linked_snapshot.generation(), &manifest.generation);
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
    let (project_database, sessions, _) = fixture.bind(&project_id).await;
    let projection = projection("resume-journaled");
    let manifest = manifest(&projection, "resume-journaled", "1");

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
    let replay = manifest
        .relational_replay(
            shard_id,
            key("resume-journaled"),
            GraphPublicationInputDigestV1::new(format!("sha256:{}", "a".repeat(64)))
                .expect("input digest"),
            None,
            &|| Ok(()),
        )
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

    let port = sessions
        .project_graph_runtime()
        .expect("bound project graph runtime")
        .as_ref();
    let published = publish_through_trait(port, &manifest, key("resume-journaled"), false)
        .expect("journaled publication must resume to a verified snapshot");
    assert_eq!(published.generation(), &manifest.generation);
    let snapshot = snapshot_through_trait(port, &projection)
        .expect("verified snapshot after the resume")
        .expect("published verified head");
    assert_eq!(snapshot.generation(), &manifest.generation);
}

#[tokio::test]
async fn registry_drop_cancels_retained_trait_runtime_operations() {
    let fixture = ContractFixture::new("lifecycle-cancellation").await;
    let project_id = project_id("lifecycle-cancellation");
    let (_, sessions, runtime) = fixture.bind(&project_id).await;
    let projection = projection("lifecycle-cancellation");
    let manifest = manifest(&projection, "lifecycle-cancellation", "1");
    assert!(sessions.project_graph_runtime().is_some());

    drop(fixture);

    assert!(matches!(
        publish_through_trait(
            runtime.as_ref(),
            &manifest,
            key("lifecycle-cancellation"),
            false,
        ),
        Err(GraphDbError::Cancelled)
    ));
}

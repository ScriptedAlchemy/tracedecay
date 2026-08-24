//! Direct persistent-activation coverage for sealed code generations.
//!
//! Production code-index activation flows through
//! [`super::RetainedCodeGraphRuntimeV1::publish_verified_snapshot`], while the
//! scheduler test registry mounts the in-memory activation authority, so this
//! path had no direct regression. The live failure it now covers: every
//! sealed publication answered `code graph database conflict` immediately
//! after its own journal append, the retained-seat and reconcile retries
//! looped on the same conflict, and the served census stayed
//! `exact_scope_generation_not_ready` until the store was reset.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use tracedecay_domain::{ProjectId, UtcMicros, canonical_sha256};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphProjectorRevision, SealedCodeGenerationReplay,
};
use tracedecay_store::{
    GraphGenerationIdV1, GraphProjectionIdV1, GraphProjectionIdentityV1,
    GraphPublicationIdempotencyKeyV1, GraphPublicationInputDigestV1, GraphPublicationKeyV1,
    GraphPublicationOperationContextV1, GraphPublicationReplayLookupV1, GraphPublicationStoreV1,
    GraphReplayAppendOutcomeV1, RetainedGraphStoreLeaseV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeRequestControlV1,
};
use tracedecay_usecases::retention::code_index_generations::DurablePublicationPointerV1;

use super::super::DaemonSessionRuntimeRegistryV1;
use super::{AtomicGraphCancellationV1, GraphPublicationProbeV1, RetainedCodeGraphRuntimeV1};
use crate::daemon::code_index_scheduler::{
    CodeGraphReplayBindingV1, CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
    scoped_code_index_store_root,
};
use crate::daemon::profile_identity;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git fixture command failed: {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn with_publication_context<T>(
    label: &str,
    operation: impl FnOnce(&GraphPublicationOperationContextV1<'_>) -> T,
) -> T {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new(format!("{label}-cancellation"))
            .expect("test cancellation id"),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new(format!("{label}-deadline"))
            .expect("test deadline id"),
    };
    let request_cancelled = Arc::new(AtomicBool::new(false));
    let request_cancellation: Arc<dyn GraphCancellation> = Arc::new(
        AtomicGraphCancellationV1::new(Arc::clone(&request_cancelled)),
    );
    let probe = GraphPublicationProbeV1 {
        request_cancellation,
        lifecycle_cancelled: Arc::new(AtomicBool::new(false)),
        deadline_at: Instant::now() + Duration::from_secs(30),
        cancellation: cancellation.clone(),
        deadline: deadline.clone(),
        commit_started: AtomicBool::new(false),
    };
    let control = RuntimeRequestControlV1 {
        requested_at: UtcMicros(crate::tracedecay::current_timestamp()),
        deadline,
        cancellation,
    };
    let context = GraphPublicationOperationContextV1::new(&control, &probe)
        .expect("test publication context");
    operation(&context)
}

fn publication_replay(
    runtime: &RetainedCodeGraphRuntimeV1,
    generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
) -> (
    GraphProjectionIdentityV1,
    GraphPublicationKeyV1,
    tracedecay_store::GraphPublicationReplayV1,
) {
    let projector_revision = GraphProjectorRevision::try_from(
        tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
    )
    .expect("projector revision");
    let projection = tracedecay_code_index::graph_projection::code_graph_projection_identity(
        runtime.authority.namespace().clone(),
    )
    .expect("code graph projection");
    let manifest =
        tracedecay_code_index::graph_projection::build_published_code_graph_manifest_checked(
            projection.clone(),
            generation,
            &projector_revision,
            &|| Ok(()),
        )
        .expect("published graph manifest");
    let relational_projection = GraphProjectionIdentityV1 {
        shard_id: runtime.authority.binding().shard_id.clone(),
        namespace: tracedecay_store::GraphNamespaceV1::new(runtime.authority.namespace().as_str())
            .expect("relational namespace"),
        projection: GraphProjectionIdV1::new(projection.projection.as_str())
            .expect("relational projection"),
    };
    let idempotency_key = tracedecay_code_index::graph_projection::code_graph_idempotency_key(
        &runtime.generation_id,
        &projector_revision,
    )
    .expect("publication idempotency key");
    let publication_key = GraphPublicationKeyV1::new(
        relational_projection.clone(),
        GraphGenerationIdV1::new(manifest.generation.as_str()).expect("relational generation"),
        GraphPublicationIdempotencyKeyV1::new(idempotency_key.as_str())
            .expect("relational idempotency key"),
    );
    let source = SealedCodeGenerationReplay {
        repository: runtime.repository_id.clone(),
        generation: runtime.generation_id.clone(),
        sealed_state_digest: runtime.sealed_state_digest.clone(),
        projector_revision,
    };
    let input = canonical_sha256(&(
        "tracedecay.code-graph-publication-input.v1",
        &source,
        &manifest.generation,
        &manifest.source_generation,
        &manifest.watermark,
    ))
    .expect("publication input digest");
    let replay = manifest
        .relational_sealed_replay(
            runtime.authority.binding().shard_id.clone(),
            idempotency_key,
            GraphPublicationInputDigestV1::new(input.as_str()).expect("publication input digest"),
            None,
            source,
            &|| Ok(()),
        )
        .expect("sealed publication replay");
    (relational_projection, publication_key, replay)
}

fn assert_unverified_publication_state(
    runtime: &RetainedCodeGraphRuntimeV1,
    generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    expected_replay: bool,
) {
    let (projection, key, _) = publication_replay(runtime, generation);
    with_publication_context("inspect-sealed-publication", |context| {
        let mut storage = runtime
            .project_database
            .graph_publication_storage()
            .expect("graph publication storage");
        let replay = storage.replay(&key, context).expect("publication replay");
        if expected_replay {
            assert!(matches!(replay, GraphPublicationReplayLookupV1::Active(_)));
        } else {
            assert!(matches!(replay, GraphPublicationReplayLookupV1::Missing));
        }
        assert!(
            storage
                .verified_head(&projection, context)
                .expect("verified graph head")
                .is_none()
        );
    });
}

fn journal_publication_without_head(
    runtime: &RetainedCodeGraphRuntimeV1,
    generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
) {
    let (_, _, replay) = publication_replay(runtime, generation);
    with_publication_context("journal-sealed-publication", |context| {
        let mut storage = runtime
            .project_database
            .graph_publication_storage()
            .expect("graph publication storage");
        assert!(matches!(
            storage
                .append_replay(&replay, context)
                .expect("append sealed publication replay"),
            GraphReplayAppendOutcomeV1::Appended(_)
        ));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealed_generation_publishes_and_republishes_without_eager_replay_payload() {
    let temporary = tempfile::tempdir().expect("temporary fixture parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
    git(&project_root, &["init", "-q", "-b", "main"]);
    git(&project_root, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project_root,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn sealed_publication_value() -> usize { 41 }\n",
    )
    .expect("project source");
    git(&project_root, &["add", "."]);
    git(&project_root, &["commit", "-qm", "sealed fixture"]);
    let project_id = ProjectId::new("project.sealed-code-publication").expect("project id");
    crate::storage::pin_fixture_repository_identity(&project_root, project_id.as_str())
        .expect("project enrollment");
    let canonical_project = project_root.canonicalize().expect("canonical project root");

    // Seal one real generation through the production worktree scheduler.
    let store_root = root.join("code-index-store");
    let scoped_store = scoped_code_index_store_root(&store_root, &canonical_project);
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        &canonical_project,
        scoped_store.clone(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open worktree scheduler");
    scheduler.reconcile_now().expect("seal the generation");
    let latest = scheduler.latest_complete().expect("complete generation");
    let repository_id = latest.generation().snapshot().repository.clone();
    let reference = latest.generation().snapshot().reference.clone();
    let worktree_id = scheduler.identity().worktree_id().clone();
    let generation_id = latest.generation().manifest().generation_id.clone();
    drop(scheduler);
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(scoped_store.join("active-code-generation-v1.json"))
            .expect("active generation pointer"),
    )
    .expect("decode active generation pointer");
    assert_eq!(pointer.generation_id, generation_id.as_str());
    let replay_binding = || CodeGraphReplayBindingV1 {
        generations_root: scoped_store.join("code-generations-v1"),
        sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest::try_from(
            pointer.state_digest.clone(),
        )
        .expect("sealed state digest"),
    };

    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 43, "sealed code publication")
            .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_database = registry
        .project_memory(project_id.clone(), [canonical_project.clone()])
        .await
        .expect("project graph database");
    let replay_root = project_database
        .database_path()
        .with_extension("graph-replay");
    tracedecay_runtime_core::storage::PrivateStoreIo::create_private_directory(&replay_root)
        .expect("private graph replay root");
    let digest = pointer
        .state_digest
        .strip_prefix("sha256:")
        .expect("sha256 state digest");
    let foreign_destination = replay_root.join(format!("generation-{digest}.json"));
    std::fs::create_dir(&foreign_destination).expect("foreign digest-named directory");
    let sentinel = foreign_destination.join("sentinel");
    std::fs::write(&sentinel, b"foreign replay evidence").expect("foreign replay sentinel");
    let canonical_seal = scoped_store
        .join("code-generations-v1")
        .join(format!("generation-{digest}.json"));
    let intact_seal = std::fs::read(&canonical_seal).expect("canonical sealed generation");
    let mut mutated_seal = intact_seal.clone();
    let mutation_offset = mutated_seal.len() / 2;
    mutated_seal[mutation_offset] ^= 1;

    let runtime = registry
        .retain_code_graph_runtime(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            reference.clone(),
            generation_id.clone(),
            Arc::clone(&project_database),
            replay_binding(),
            // No decoded-seal offer: this suite asserts the on-disk seal
            // verification contract, so every read must reach the canonical
            // root.
            None,
        )
        .await
        .expect("retain code graph runtime");

    std::fs::write(&canonical_seal, &mutated_seal).expect("mutate sealed generation in place");
    assert!(matches!(
        runtime.publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false))),
        Err(GraphDbError::Corrupt { .. })
    ));
    assert_unverified_publication_state(&runtime, latest.generation(), false);
    std::fs::write(&canonical_seal, &intact_seal).expect("restore sealed generation bytes");

    let retained_seal = canonical_seal.with_extension("retained-test-evidence");
    std::fs::rename(&canonical_seal, &retained_seal).expect("retain original sealed inode");
    std::fs::write(&canonical_seal, &mutated_seal).expect("replace canonical sealed inode");
    assert!(matches!(
        runtime.publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false))),
        Err(GraphDbError::Corrupt { .. })
    ));
    assert_unverified_publication_state(&runtime, latest.generation(), false);
    std::fs::remove_file(&canonical_seal).expect("remove replacement sealed inode");
    std::fs::rename(&retained_seal, &canonical_seal).expect("restore original sealed inode");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        std::fs::rename(&canonical_seal, &retained_seal).expect("retain symlink target evidence");
        symlink(&retained_seal, &canonical_seal).expect("swap canonical seal for symlink");
        assert!(matches!(
            runtime
                .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false))),
            Err(GraphDbError::Corrupt { .. })
        ));
        assert_unverified_publication_state(&runtime, latest.generation(), false);
        std::fs::remove_file(&canonical_seal).expect("remove sealed generation symlink");
        std::fs::rename(&retained_seal, &canonical_seal)
            .expect("restore sealed generation after symlink refusal");
    }

    journal_publication_without_head(&runtime, latest.generation());
    std::fs::write(&canonical_seal, &mutated_seal)
        .expect("mutate source before active replay completion");
    assert!(matches!(
        runtime.publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false))),
        Err(GraphDbError::Corrupt { .. })
    ));
    assert_unverified_publication_state(&runtime, latest.generation(), true);
    std::fs::write(&canonical_seal, &intact_seal)
        .expect("restore source before active replay completion");

    let snapshot = runtime
        .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        .expect("the sealed generation must publish as the verified code graph");
    let expected_generation = tracedecay_code_index::graph_projection::code_graph_generation_id(
        &generation_id,
        &GraphProjectorRevision::try_from(
            tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
        )
        .expect("projector revision"),
    )
    .expect("code graph generation id");
    assert_eq!(snapshot.generation(), &expected_generation);
    let head = snapshot.verified_head().clone();
    drop(snapshot);
    assert_eq!(
        std::fs::read(&sentinel).expect("foreign replay evidence survives first publication"),
        b"foreign replay evidence"
    );
    let first_publish_payload_bytes = std::fs::read_dir(&replay_root)
        .expect("read graph replay root after first publication")
        .map(|entry| entry.expect("graph replay entry"))
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("generation-")
                && entry
                    .file_type()
                    .expect("graph replay entry type")
                    .is_file()
        })
        .map(|entry| {
            entry
                .metadata()
                .expect("graph replay payload metadata")
                .len()
        })
        .sum::<u64>();
    assert_eq!(first_publish_payload_bytes, 0);

    // A retained-seat retry of the same sealed artifact resumes to the same
    // verified head instead of conflicting with its own journaled replay.
    let retried = registry
        .retain_code_graph_runtime(
            project_id,
            repository_id,
            worktree_id,
            reference,
            generation_id,
            Arc::clone(&project_database),
            replay_binding(),
            // No decoded-seal offer: this suite asserts the on-disk seal
            // verification contract, so every read must reach the canonical
            // root.
            None,
        )
        .await
        .expect("retain code graph runtime again");
    let resumed = retried
        .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        .expect("a repeated activation must resume the exact publication");
    assert_eq!(resumed.generation(), &expected_generation);
    assert_eq!(resumed.verified_head(), &head);
    assert_eq!(
        std::fs::read(&sentinel).expect("foreign replay evidence survives active republish"),
        b"foreign replay evidence"
    );
    let active_republish_payload_bytes = std::fs::read_dir(&replay_root)
        .expect("read graph replay root after active republish")
        .map(|entry| entry.expect("graph replay entry"))
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("generation-")
                && entry
                    .file_type()
                    .expect("graph replay entry type")
                    .is_file()
        })
        .map(|entry| {
            entry
                .metadata()
                .expect("graph replay payload metadata")
                .len()
        })
        .sum::<u64>();
    assert_eq!(active_republish_payload_bytes, 0);
}

/// Stage 1 of `docs/plans/tracedecay-v2/40`: cold activation decodes the sealed
/// payload once to serve queries, and graph hydration reuses that decode
/// instead of reading and parsing the identical bytes a second time.
///
/// The assertion is falsifiable by construction rather than by timing: BOTH
/// seal roots handed to the provider are empty, so hydration can only succeed
/// by consuming the offered decode. The first probe proves the roots really are
/// unreadable, and the last probe proves a foreign sealed digest is never
/// answered from the offer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offered_decode_hydrates_without_reading_the_sealed_payload_again() {
    use tracedecay_graph_db::{
        GraphGenerationManifestProvider, GraphNamespace, SealedGraphStateDigest,
    };
    use tracedecay_store::{BrainId, GraphNamespaceV1, StoreShardIdV1, UserProfileId};

    use super::super::code_graph_manifest::DaemonCodeGraphManifestProviderV1;

    let temporary = tempfile::tempdir().expect("temporary fixture parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let project_root = root.join("project");
    std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
    git(&project_root, &["init", "-q", "-b", "main"]);
    git(&project_root, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project_root,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn offered_decode_value() -> usize { 7 }\n",
    )
    .expect("project source");
    git(&project_root, &["add", "."]);
    git(&project_root, &["commit", "-qm", "offered decode fixture"]);
    let project_id = ProjectId::new("project.offered-decode").expect("project id");
    crate::storage::pin_fixture_repository_identity(&project_root, project_id.as_str())
        .expect("project enrollment");
    let canonical_project = project_root.canonicalize().expect("canonical project root");

    // Seal one real generation through the production worktree scheduler, then
    // take the exact handle the code index would serve queries from.
    let store_root = root.join("code-index-store");
    let scoped_store = scoped_code_index_store_root(&store_root, &canonical_project);
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        &canonical_project,
        scoped_store.clone(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open worktree scheduler");
    scheduler.reconcile_now().expect("seal the generation");
    let latest = scheduler.latest_complete().expect("complete generation");
    let decoded = latest.generation_handle();
    let generation_id = decoded.manifest().generation_id.clone();
    let repository_id = decoded.snapshot().repository.clone();
    drop(scheduler);

    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(scoped_store.join("active-code-generation-v1.json"))
            .expect("active generation pointer"),
    )
    .expect("decode active generation pointer");
    let sealed_state_digest = SealedGraphStateDigest::try_from(pointer.state_digest.clone())
        .expect("sealed state digest");

    // Deliberately empty: the provider has nothing it could read from either the
    // canonical root or the replay pool.
    let absent_generations_root = root.join("absent-generations");
    let absent_replay_root = root.join("absent-replay");
    std::fs::create_dir_all(&absent_generations_root).expect("absent generations root");
    std::fs::create_dir_all(&absent_replay_root).expect("absent replay root");

    let shard = StoreShardIdV1::project(
        BrainId::new("brain.offered-decode").expect("brain id"),
        UserProfileId::new("profile.offered-decode").expect("profile id"),
        project_id.clone(),
    );
    let provider = DaemonCodeGraphManifestProviderV1::default();
    provider
        .bind(
            shard.clone(),
            project_id.clone(),
            repository_id.clone(),
            absent_generations_root,
            absent_replay_root,
        )
        .expect("bind code generation source");

    let namespace = GraphNamespace::new("namespace.offered-decode").expect("graph namespace");
    let projection =
        tracedecay_code_index::graph_projection::code_graph_projection_identity(namespace.clone())
            .expect("code graph projection");
    let owner = GraphProjectionIdentityV1 {
        shard_id: shard.clone(),
        namespace: GraphNamespaceV1::new(namespace.as_str()).expect("relational namespace"),
        projection: GraphProjectionIdV1::new(projection.projection.as_str())
            .expect("relational projection"),
    };
    let source = SealedCodeGenerationReplay {
        repository: repository_id.clone(),
        generation: generation_id.clone(),
        sealed_state_digest: sealed_state_digest.clone(),
        projector_revision: GraphProjectorRevision::try_from(
            tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
        )
        .expect("projector revision"),
    };

    // The seal really is unreadable from both roots, so any success below is
    // evidence that the offered decode was consumed.
    provider
        .hydrate_sealed_code_generation(&owner, &source, &|| Ok(()))
        .expect_err("hydration must fail while no seal is readable and nothing is offered");

    provider
        .offer_decoded_code_generation(
            shard.clone(),
            generation_id.clone(),
            sealed_state_digest.clone(),
            Arc::clone(&decoded),
        )
        .expect("offer the decoded generation");
    let manifest = provider
        .hydrate_sealed_code_generation(&owner, &source, &|| Ok(()))
        .expect("hydration reuses the offered decode without reading the seal");
    assert_eq!(manifest.projection.namespace.as_str(), namespace.as_str());

    // A different sealed payload must never be answered from this offer.
    let foreign = SealedCodeGenerationReplay {
        sealed_state_digest: SealedGraphStateDigest::try_from(format!("sha256:{}", "b".repeat(64)))
            .expect("foreign sealed digest"),
        ..source.clone()
    };
    provider
        .hydrate_sealed_code_generation(&owner, &foreign, &|| Ok(()))
        .expect_err("a foreign sealed digest must never be served from the offer");
}

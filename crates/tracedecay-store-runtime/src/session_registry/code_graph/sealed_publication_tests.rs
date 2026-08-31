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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracedecay_code_index_retention::code_index_generations::DurablePublicationPointerV1;
use tracedecay_domain::{ProjectId, canonical_sha256};
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

use super::super::DaemonSessionRuntimeRegistryV1;
use super::{AtomicGraphCancellationV1, GraphPublicationProbeV1, RetainedCodeGraphRuntimeV1};
use tracedecay_code_index_runtime::CodeGraphReplayBindingV1;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1, scoped_code_index_store_root,
};
use tracedecay_daemon_identity::profile_identity;

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
        lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::new(
            AtomicBool::new(false),
        ))),
        deadline_at: Instant::now() + Duration::from_secs(30),
        cancellation: cancellation.clone(),
        deadline: deadline.clone(),
        commit_started: AtomicBool::new(false),
        deadline_warned: AtomicBool::new(false),
    };
    let control = RuntimeRequestControlV1 {
        requested_at: tracedecay_application::clock::now_micros(),
        deadline,
        cancellation,
    };
    // 2020-01-01T00:00:00Z in micros. A seconds-scale stamp (~1.8e9) fails
    // this bound — the 1970-era seconds-as-micros regression this pins.
    assert!(
        control.requested_at.0 > 1_577_836_800_000_000,
        "requested_at must be micros-scale, got {}",
        control.requested_at.0
    );
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
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
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
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        43,
        "sealed code publication",
    )
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

    // The issue-765 wedge shape: the journal above already carries this
    // publication's active replay — the debris an interrupted publisher
    // leaves behind. First activation must resume that journaled replay and
    // publish in ONE call, regardless of sealed artifact size: no
    // manufactured stage-boundary `DeadlineExceeded`, no scheduler retry
    // pass, no conflict against its own journal.
    let snapshot = runtime
        .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        .expect("first activation must resume the journaled replay and publish in one call");
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

    // A subsequent generation must equally stage, reopen, and advance its
    // head in one call.
    drop(resumed);
    drop(retried);
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn sealed_publication_value() -> usize { 42 }\n",
    )
    .expect("updated project source");
    git(&project_root, &["add", "."]);
    git(&project_root, &["commit", "-qm", "update sealed fixture"]);
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        &canonical_project,
        scoped_store.clone(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("reopen worktree scheduler");
    scheduler.reconcile_now().expect("seal next generation");
    let next = scheduler
        .latest_complete()
        .expect("next complete generation");
    let next_pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(scoped_store.join("active-code-generation-v1.json"))
            .expect("next active generation pointer"),
    )
    .expect("decode next active generation pointer");
    let next_binding = CodeGraphReplayBindingV1 {
        generations_root: scoped_store.join("code-generations-v1"),
        sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest::try_from(
            next_pointer.state_digest,
        )
        .expect("next sealed state digest"),
    };
    let mut next_runtime = registry
        .retain_code_graph_runtime(
            project_id,
            next.generation().snapshot().repository.clone(),
            scheduler.identity().worktree_id().clone(),
            next.generation().snapshot().reference.clone(),
            next.generation().manifest().generation_id.clone(),
            project_database,
            next_binding,
            None,
        )
        .await
        .expect("retain next code graph runtime");
    let next_snapshot = next_runtime
        .publish_verified_snapshot(next.generation(), Arc::new(AtomicBool::new(false)))
        .expect("a fresh small projection must publish in one call");
    assert_eq!(
        next_snapshot.generation().as_str(),
        tracedecay_code_index::graph_projection::code_graph_generation_id(
            &next.generation().manifest().generation_id,
            &GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )
            .expect("next projector revision"),
        )
        .expect("next projected generation")
        .as_str()
    );

    // A cold daemon has the durable relational head and the derived sealed
    // graph store produced by the successful publication above, but no shared
    // staging runtime in its new GraphDB registry. Reopening the exact active
    // publication must use that verified sealed artifact directly and leave
    // the staging shard unregistered.
    let next_generation = next_snapshot.generation().clone();
    let next_head = next_snapshot.verified_head().clone();
    drop(next_snapshot);
    let manifest_provider: Arc<dyn tracedecay_graph_db::GraphGenerationManifestProvider> =
        next_runtime.graph_manifest_provider.clone();
    let cold_graph_registry = tracedecay_graph_db::GraphDbRegistry::new_with_manifest_provider(
        tracedecay_graph_db::GraphDbRegistryConfig { max_open: 1 },
        manifest_provider,
    )
    .expect("cold graph registry");
    // This test retains graph runtimes directly outside the daemon owner maps.
    // Model the real process boundary by dropping every old-daemon runtime and
    // registry owner, then seat the retained publication inputs in a fresh
    // lifecycle and GraphDB registry. The old registry's final drop releases
    // its staging and sealed Grafeo handles without bypassing lease checks.
    drop(runtime);
    drop(registry);
    next_runtime.lifecycle_cancelled = Arc::new(AtomicBool::new(false));
    drop(std::mem::replace(
        &mut next_runtime.graph_registry,
        cold_graph_registry.clone(),
    ));
    assert!(
        !cold_graph_registry
            .shard_is_registered(&next_runtime.authority.binding().shard_id)
            .expect("cold staging registration state")
    );
    let cold_reopened = next_runtime
        .publish_verified_snapshot(next.generation(), Arc::new(AtomicBool::new(false)))
        .expect("cold activation must recover from the verified sealed artifact");
    assert_eq!(cold_reopened.generation(), &next_generation);
    assert_eq!(cold_reopened.verified_head(), &next_head);
    assert!(
        !cold_graph_registry
            .shard_is_registered(&next_runtime.authority.binding().shard_id)
            .expect("post-recovery staging registration state"),
        "direct sealed recovery must not mount the shared staging database"
    );
}

/// The sealed read bundle journey over the production seal/open path:
///
/// - sealing (first successful publication) writes the bundle manifest and
///   the interactive-catalog artifact next to the sealed generation;
/// - open loads the digest-verified catalog and installs it WITHOUT running
///   the projection warm scan (the scan counter proves no warm work ran);
/// - a tampered artifact is the typed `Stale` state, a removed bundle is the
///   typed `Absent` state, and in both cases the explicit fallback — the
///   projection warm scan — still serves the catalog;
/// - retirement removes every bundle file for the generation's digest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealed_read_bundle_serves_catalog_without_warm_and_degrades_typed() {
    use tracedecay_code_index::graph_projection::CodeGraphProjectionStore;
    use tracedecay_graph_db::SealedReadBundleArtifactStateV1;

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
        "pub fn sealed_bundle_value() -> usize { 43 }\n",
    )
    .expect("project source");
    git(&project_root, &["add", "."]);
    git(&project_root, &["commit", "-qm", "sealed bundle fixture"]);
    let project_id = ProjectId::new("project.sealed-read-bundle").expect("project id");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let canonical_project = project_root.canonicalize().expect("canonical project root");

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
    let generations_root = scoped_store.join("code-generations-v1");
    let sealed_state_digest =
        tracedecay_graph_db::SealedGraphStateDigest::try_from(pointer.state_digest.clone())
            .expect("sealed state digest");
    let digest_hex = pointer
        .state_digest
        .strip_prefix("sha256:")
        .expect("sha256 state digest")
        .to_owned();
    let bundle_manifest_path = generations_root.join(format!("read-bundle-{digest_hex}.json"));
    let bundle_catalog_path =
        generations_root.join(format!("read-bundle-{digest_hex}.interactive-catalog.bin"));

    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        44,
        "sealed read bundle",
    )
    .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_database = registry
        .project_memory(project_id.clone(), [canonical_project.clone()])
        .await
        .expect("project graph database");
    let runtime = registry
        .retain_code_graph_runtime(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            reference.clone(),
            generation_id.clone(),
            Arc::clone(&project_database),
            CodeGraphReplayBindingV1 {
                generations_root: generations_root.clone(),
                sealed_state_digest: sealed_state_digest.clone(),
            },
            None,
        )
        .await
        .expect("retain code graph runtime");

    assert!(
        !bundle_manifest_path.exists(),
        "no bundle may exist before the generation's graph is sealed"
    );
    let snapshot = runtime
        .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        .expect("seal the code graph");

    // Seal produced the bundle.
    assert!(
        bundle_manifest_path.is_file(),
        "sealing must write the read bundle manifest"
    );
    assert!(
        bundle_catalog_path.is_file(),
        "sealing must write the interactive-catalog artifact"
    );

    // Open of a bundled generation loads the catalog and skips the warm.
    let loaded = runtime
        .load_sealed_read_bundle_catalog(&Arc::new(AtomicBool::new(false)))
        .expect("load the bundle catalog");
    let SealedReadBundleArtifactStateV1::Loaded { artifact, bytes } = loaded else {
        panic!("a freshly sealed bundle must load, got {loaded:?}");
    };
    assert_eq!(artifact.name, "interactive-catalog");
    let store = CodeGraphProjectionStore::from_verified_snapshot(snapshot, generation_id.clone())
        .expect("projection store over the sealed snapshot");
    store
        .install_interactive_catalog_artifact(&bytes, Arc::new(tracedecay_graph_db::NeverCancelled))
        .expect("install the bundled catalog");
    assert!(
        store
            .interactive_catalog_is_warm()
            .expect("catalog state readable"),
        "a bundled generation opens with a ready catalog"
    );
    assert_eq!(
        store.interactive_catalog_scan_builds(),
        0,
        "opening a bundled generation must not run the projection warm scan"
    );

    // A tampered artifact is the typed stale state, never a silent load.
    let intact_artifact = std::fs::read(&bundle_catalog_path).expect("read catalog artifact");
    std::fs::write(&bundle_catalog_path, b"tampered").expect("tamper catalog artifact");
    let stale = runtime
        .load_sealed_read_bundle_catalog(&Arc::new(AtomicBool::new(false)))
        .expect("stale load is a typed state, not an error");
    assert!(
        matches!(stale, SealedReadBundleArtifactStateV1::Stale { .. }),
        "tampered artifact bytes must be typed stale, got {stale:?}"
    );
    std::fs::write(&bundle_catalog_path, &intact_artifact).expect("restore catalog artifact");

    // Retirement removes the bundle with its generation; the generation then
    // reads as an old, bundle-less seal: typed absent, served by the explicit
    // warm fallback.
    tracedecay_graph_db::retire_sealed_read_bundle(&generations_root, &sealed_state_digest)
        .expect("retire the read bundle");
    assert!(!bundle_manifest_path.exists());
    assert!(!bundle_catalog_path.exists());
    let absent = runtime
        .load_sealed_read_bundle_catalog(&Arc::new(AtomicBool::new(false)))
        .expect("absent load is a typed state, not an error");
    assert!(
        matches!(absent, SealedReadBundleArtifactStateV1::Absent { .. }),
        "a bundle-less generation must be typed absent, got {absent:?}"
    );
    let fallback_snapshot = runtime
        .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        .expect("republish resumes the verified head");
    let fallback_store =
        CodeGraphProjectionStore::from_verified_snapshot(fallback_snapshot, generation_id.clone())
            .expect("projection store for the fallback");
    fallback_store
        .mark_interactive_catalog_warming()
        .expect("mark warming");
    fallback_store
        .warm_interactive_catalog_with_cancellation(Arc::new(tracedecay_graph_db::NeverCancelled))
        .expect("the old-generation fallback warm must still serve");
    assert_eq!(
        fallback_store.interactive_catalog_scan_builds(),
        1,
        "the fallback path is the explicit projection re-derivation"
    );
    assert!(
        fallback_store
            .interactive_catalog_is_warm()
            .expect("fallback catalog state readable"),
        "an old generation without a bundle still serves via the warm"
    );
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
        GraphGenerationManifest, GraphGenerationManifestProvider, GraphNamespace,
        SealedGraphStateDigest,
    };
    use tracedecay_store::{
        BrainId, GraphNamespaceV1, GraphPublicationInputDigestV1, StoreShardIdV1, UserProfileId,
    };

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
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
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

    // A pending predecessor owns the projector revision recorded in its
    // durable replay, even after the current reader has advanced. Rebuild that
    // exact historical manifest and prove the replay digests accept it; this
    // is what lets an interrupted predecessor finish before the current
    // publication appends.
    let legacy_revision = GraphProjectorRevision::try_from("code-graph-projector.v4".to_owned())
        .expect("persisted predecessor revision");
    let legacy_source = SealedCodeGenerationReplay {
        projector_revision: legacy_revision.clone(),
        ..source.clone()
    };
    let legacy_manifest =
        tracedecay_code_index::graph_projection::build_published_code_graph_manifest_checked(
            projection,
            &decoded,
            &legacy_revision,
            &|| Ok(()),
        )
        .expect("build the predecessor manifest");
    let legacy_replay = legacy_manifest
        .relational_sealed_replay(
            shard,
            tracedecay_code_index::graph_projection::code_graph_idempotency_key(
                &generation_id,
                &legacy_revision,
            )
            .expect("predecessor idempotency key"),
            GraphPublicationInputDigestV1::new(format!("sha256:{}", "c".repeat(64)))
                .expect("predecessor input digest"),
            None,
            legacy_source,
            &|| Ok(()),
        )
        .expect("predecessor relational replay");
    let reconstructed = GraphGenerationManifest::from_replay(&legacy_replay, &provider, &|| Ok(()))
        .expect("the exact historical predecessor must hydrate and verify");
    assert_eq!(reconstructed, *legacy_manifest);

    // A different sealed payload must never be answered from this offer.
    let foreign = SealedCodeGenerationReplay {
        sealed_state_digest: SealedGraphStateDigest::try_from(format!("sha256:{}", "b".repeat(64)))
            .expect("foreign sealed digest"),
        ..source.clone()
    };
    provider
        .hydrate_sealed_code_generation(&owner, &foreign, &|| Ok(()))
        .expect_err("a foreign sealed digest must never be served from the offer");

    // Both consumers above were served from the one offer, which is exactly why
    // the offer is not taken on first read. Its lifetime bound is retirement.
    assert_eq!(
        provider.retained_decoded_offer_count(),
        1,
        "the offer survives its consumers so the predecessor path can reuse it"
    );
    let census_bytes = provider.retained_decoded_offer_bytes();
    assert!(
        census_bytes > 0,
        "a retained offer reports the sealed source census it holds"
    );

    // Retirement releases it. Before this, nothing removed an offer at all.
    let retirement_shard = owner.shard_id.clone();
    assert_eq!(
        provider.release_decoded_offer(&retirement_shard),
        census_bytes
    );
    assert_eq!(provider.retained_decoded_offer_count(), 0);
    assert_eq!(provider.retained_decoded_offer_bytes(), 0);
    provider
        .hydrate_sealed_code_generation(&owner, &source, &|| Ok(()))
        .expect_err("a released offer falls back to the canonical seal, which is unreadable here");

    // Pressure backstop, driven by an injected measured-RSS series on an
    // isolated cell: no `/proc` read, and no interference with other cases.
    let pressure = std::sync::Arc::new(
        tracedecay_runtime_core::resident_memory::ResidentMemoryPressureV1::new(
            std::num::NonZeroU64::new(1024 * 1024 * 1024).expect("nonzero pressure limit"),
        ),
    );
    let pressured = DaemonCodeGraphManifestProviderV1::with_pressure(&pressure);
    pressured
        .offer_decoded_code_generation(
            retirement_shard.clone(),
            generation_id.clone(),
            sealed_state_digest.clone(),
            Arc::clone(&decoded),
        )
        .expect("offer the decoded generation to the pressured provider");
    assert_eq!(pressured.retained_decoded_offer_count(), 1);

    pressure.publish_observed_resident_bytes(pressure.low_watermark_bytes());
    assert_eq!(
        pressured.retained_decoded_offer_count(),
        1,
        "nominal measured RSS keeps the accelerator"
    );

    pressure.publish_observed_resident_bytes(pressure.high_watermark_bytes() + 1);
    assert_eq!(
        pressured.retained_decoded_offer_count(),
        0,
        "measured RSS over the high watermark drops the retained decode"
    );
    assert_eq!(pressured.retained_decoded_offer_bytes(), 0);
}

/// The per-shard publication gate is one shared cell across retained runtime
/// instances: the seat pass and the background reconcile publishing the same
/// sealed generation serialize instead of racing the graph database into a
/// Conflict, and the loser resumes the winner's exact verified head through
/// the idempotent recovery arm. A request cancelled while another instance
/// holds the gate answers typed cancellation without mutating the journal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_sealed_publishers_share_one_gate_and_converge_on_one_head() {
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
        "pub fn concurrent_publication_value() -> usize { 43 }\n",
    )
    .expect("project source");
    git(&project_root, &["add", "."]);
    git(
        &project_root,
        &["commit", "-qm", "concurrent publication fixture"],
    );
    let project_id = ProjectId::new("project.concurrent-code-publication").expect("project id");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
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
    let replay_binding = || CodeGraphReplayBindingV1 {
        generations_root: scoped_store.join("code-generations-v1"),
        sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest::try_from(
            pointer.state_digest.clone(),
        )
        .expect("sealed state digest"),
    };

    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        47,
        "concurrent code publication",
    )
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

    let seat = registry
        .retain_code_graph_runtime(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            reference.clone(),
            generation_id.clone(),
            Arc::clone(&project_database),
            replay_binding(),
            None,
        )
        .await
        .expect("retain the seat-pass code graph runtime");
    let reconcile = registry
        .retain_code_graph_runtime(
            project_id,
            repository_id,
            worktree_id,
            reference,
            generation_id,
            project_database,
            replay_binding(),
            None,
        )
        .await
        .expect("retain the reconcile code graph runtime");
    // Cross-instance exclusivity is one registry-owned lock cell per code
    // shard; per-instance locks would silently reintroduce the publication
    // race the flight table and gate exist to prevent.
    assert!(Arc::ptr_eq(
        &seat.publication_locks,
        &reconcile.publication_locks
    ));

    // A publisher cancelled while another instance holds the serving gate
    // answers typed cancellation and leaves the publication journal
    // untouched: the classification slice blocks on the gate before any
    // journal read or write, and observes the typed interruption first thing
    // after acquiring it.
    let cancelled = Arc::new(AtomicBool::new(false));
    let held = seat
        .publication_locks
        .gate
        .lock()
        .expect("hold the publication gate");
    let outcome = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            reconcile.publish_verified_snapshot(latest.generation(), Arc::clone(&cancelled))
        });
        cancelled.store(true, Ordering::Release);
        drop(held);
        worker.join().expect("join the cancelled publisher")
    });
    assert!(matches!(outcome, Err(GraphDbError::Cancelled)));
    assert_unverified_publication_state(&reconcile, latest.generation(), false);

    // The seat pass and the background reconcile publish the same sealed
    // generation concurrently: the loser waits out the winner, then resumes
    // the winner's exact publication instead of conflicting.
    let (seat_outcome, reconcile_outcome) = std::thread::scope(|scope| {
        let seat_worker = scope.spawn(|| {
            seat.publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        });
        let reconcile_worker = scope.spawn(|| {
            reconcile
                .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        });
        (
            seat_worker.join().expect("join the seat publisher"),
            reconcile_worker
                .join()
                .expect("join the reconcile publisher"),
        )
    });
    let seat_snapshot = seat_outcome.expect("seat publication");
    let reconcile_snapshot = reconcile_outcome.expect("reconcile publication");
    assert_eq!(seat_snapshot.generation(), reconcile_snapshot.generation());
    assert_eq!(
        seat_snapshot.verified_head(),
        reconcile_snapshot.verified_head()
    );

    // The verified head advanced exactly once and the journal retains exactly
    // the winner's active replay: the loser recovered the published head
    // rather than appending a duplicate or double-advancing the head.
    let (projection, key, _) = publication_replay(&seat, latest.generation());
    with_publication_context("inspect-converged-publication", |context| {
        let mut storage = seat
            .project_database
            .graph_publication_storage()
            .expect("graph publication storage");
        let head = storage
            .verified_head(&projection, context)
            .expect("verified graph head")
            .expect("converged verified head");
        assert_eq!(&head, seat_snapshot.verified_head());
        assert!(matches!(
            storage.replay(&key, context).expect("publication replay"),
            GraphPublicationReplayLookupV1::Active(_)
        ));
    });
}

//! Isolated daemon tests for the vector-retention inventory states that
//! drive the code-generation retention pass: the quiet default-off journey,
//! census paging, the degraded offline inventory, and the fail-closed
//! refusals. Fixtures pin the user data dir and stay inside `TempDir`s.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::daemon::maintenance::{
    SemanticVectorRetentionCensusOutcome, SemanticVectorRetentionReadV1,
    StoreTelemetrySamplingRegistry,
};
use crate::daemon::store_writer_gate::{StoreWriterGates, WriterScope};
use crate::tracedecay::TraceDecay;
use tracedecay_code_index_retention::code_index_generations::{
    CodeGenerationRetentionErrorV1, CodeGenerationRetentionModeV1,
    DEFAULT_SUPERSEDED_GENERATION_FLOOR, DurableGenerationIndexEntryV1,
    DurablePublicationPointerV1, GRAPH_REPLAY_POOL_ACQUIRE_BUDGET, durable_generation_index_digest,
    execute_code_generation_retention, plan_code_generation_retention,
    try_acquire_code_generation_store_lock,
};
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_code_index_runtime::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources;
use tracedecay_domain::UtcMicros;
use tracedecay_semantic_contracts::{
    DEFAULT_FASTEMBED_MODEL_ID, SemanticConfig, SemanticProfileSelection, SemanticResourceCeilings,
};

use super::{
    CodeGenerationRetentionOutcomeV1, VectorRetentionInventoryV1, apply_code_generation_retention,
    classify_vector_readable_sources, code_index_store_root, resolve_vector_retention_inventory,
    run_code_generation_retention, run_semantic_vector_generation_retention,
};

const FIXTURE_GENERATION_COUNT: usize = 6;

struct UnseatedGraphFixture {
    _pinned_home: tracedecay_runtime_core::config::PinnedUserDataDir,
    _project: TempDir,
    graph: TraceDecay,
    store_root: PathBuf,
    schedulers: CodeIndexSchedulerRegistryV1,
    observations: StoreTelemetrySamplingRegistry,
    cancellation: tracedecay_session_memory::context::CancellationToken,
}

/// A mounted project graph whose daemon never seated a semantic runtime — the
/// live default-off state — with a sealed code-index store that has
/// collectable superseded generations.
async fn open_unseated_graph_fixture() -> UnseatedGraphFixture {
    let pinned_home = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = TempDir::new().expect("isolated project root");
    let graph = TraceDecay::open(project.path())
        .await
        .expect("open isolated project graph");
    assert!(
        graph
            .configuration_runtime()
            .semantic_configuration_inventory_authority()
            .is_none(),
        "fixture daemon must have no seated semantic runtime"
    );
    let layout = graph.hook_store_layout();
    let store_root = code_index_store_root(&layout.data_root, &layout.project_root);
    seed_sealed_generation_store(&store_root, FIXTURE_GENERATION_COUNT);
    UnseatedGraphFixture {
        _pinned_home: pinned_home,
        _project: project,
        graph,
        store_root,
        schedulers: CodeIndexSchedulerRegistryV1::new(1),
        observations: StoreTelemetrySamplingRegistry::default(),
        cancellation: tracedecay_session_memory::context::CancellationToken::new(),
    }
}

/// Seed a valid sealed code-index generation store: `count` sealed generation
/// files whose names match their content digests, and an active publication
/// pointer at the newest one. Every older generation is superseded and, with
/// no vector pins, collectable one bounded unit per pass.
fn seed_sealed_generation_store(store_root: &Path, count: usize) {
    let generations_root = store_root.join("code-generations-v1");
    std::fs::create_dir_all(&generations_root).expect("create generation directory");
    let mut sealed = Vec::with_capacity(count);
    for sequence in 0..count {
        let generation_id = format!("generation.v1.retention-fixture.{sequence:08}");
        let sealed_at = i64::try_from(sequence).expect("sequence fits i64");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "format_revision":
                tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1,
            "manifest": {
                "generation_id": generation_id,
                "seal": { "sealed_at": sealed_at },
            },
            "chunks": [],
        }))
        .expect("serialize sealed generation fixture");
        let state_digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
        let file = format!(
            "generation-{}.json",
            state_digest.strip_prefix("sha256:").expect("digest prefix")
        );
        let size_bytes = u64::try_from(bytes.len()).expect("size fits u64");
        std::fs::write(generations_root.join(&file), bytes).expect("write sealed generation");
        sealed.push((generation_id, file, state_digest, size_bytes, sealed_at));
    }
    let (generation_id, file, state_digest, size_bytes, sealed_at) = sealed
        .last()
        .expect("at least one sealed generation")
        .clone();
    let generation_index = vec![DurableGenerationIndexEntryV1 {
        generation_id: generation_id.clone(),
        snapshot_content_identity: "snapshot.retention-fixture".to_owned(),
        sealed_at_micros: sealed_at,
        size_bytes,
        generation_file: file.clone(),
        state_digest: state_digest.clone(),
        source_reference: None,
        source_revision: None,
        source_tree: None,
        cardinality: None,
        text_artifact: None,
    }];
    let generation_index_digest =
        durable_generation_index_digest(&generation_index, true).expect("index digest");
    let pointer = DurablePublicationPointerV1 {
        generation_id,
        snapshot_content_identity: "snapshot.retention-fixture".to_owned(),
        publication_digest: "sha256:publication".to_owned(),
        sealed_at_micros: sealed_at,
        generation_file: file,
        state_digest,
        generation_index,
        generation_index_truncated: true,
        generation_index_digest: Some(generation_index_digest),
    };
    std::fs::write(
        store_root.join("active-code-generation-v1.json"),
        serde_json::to_vec(&pointer).expect("serialize active pointer"),
    )
    .expect("write active pointer");
}

fn sealed_generation_files(store_root: &Path) -> BTreeSet<String> {
    std::fs::read_dir(store_root.join("code-generations-v1"))
        .expect("read generation directory")
        .map(|entry| {
            entry
                .expect("generation directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with("generation-"))
        .collect()
}

fn active_generation_file(store_root: &Path) -> String {
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(store_root.join("active-code-generation-v1.json"))
            .expect("read active pointer"),
    )
    .expect("decode active pointer");
    pointer.generation_file
}

fn fixture_census_shard() -> (
    tracedecay_store::StoreShardIdV1,
    tracedecay_store::SemanticVectorStageCensusRevision,
) {
    (
        tracedecay_store::StoreShardIdV1::project(
            tracedecay_domain::BrainId::new("brain.retention-fixture").expect("brain id"),
            tracedecay_domain::UserProfileId::new("profile.retention-fixture").expect("profile id"),
            tracedecay_domain::ProjectId::new("project.retention-fixture").expect("project id"),
        ),
        tracedecay_store::SemanticVectorStageCensusRevision::new(1).expect("census revision"),
    )
}

/// Drive the registry into `Scanning`: one bounded census page with a
/// continuation cursor and no complete receipt.
fn record_paging_census(observations: &StoreTelemetrySamplingRegistry, project_root: &Path) {
    let (shard_id, revision) = fixture_census_shard();
    let counts = tracedecay_store::SemanticVectorStageCensusCounts {
        pending: 1,
        ready: 0,
        published: 1,
        cancelled: 0,
    };
    let cursor = tracedecay_store::SemanticVectorStageCensusCursor::new(
        shard_id.clone(),
        None,
        revision,
        256,
        counts,
        tracedecay_domain::canonical_sha256(&"retention-fixture-page").expect("page digest"),
    )
    .expect("census continuation cursor");
    let census = tracedecay_graph_db::SemanticVectorRetentionCensus {
        shard_id,
        revision,
        pending: 1,
        ready: 0,
        published: 1,
        cancelled: 0,
        complete_receipt: None,
        continuation: Some(cursor),
        action: tracedecay_graph_db::SemanticVectorRetentionAction::None,
    };
    assert_eq!(
        observations.record_semantic_vector_retention_census(project_root, &census),
        SemanticVectorRetentionCensusOutcome::Accepted
    );
    assert_eq!(
        observations.semantic_vector_retention_read(project_root),
        SemanticVectorRetentionReadV1::Scanning
    );
}

#[test]
fn committed_retrieval_profiles_keep_the_unseated_state_retryable() {
    let selection = SemanticProfileSelection {
        profile_id: "profile.retention-fixture".to_owned(),
        accepted_profile_digest: tracedecay_domain::ManifestDigest::new(format!(
            "sha256:{}",
            "a".repeat(64)
        ))
        .expect("accepted profile digest"),
        artifact_digest: format!("sha256:{}", "b".repeat(64)),
        artifact_path: PathBuf::from("/fixture/semantic-model"),
    };
    let disabled = SemanticConfig {
        selected_model: Some(DEFAULT_FASTEMBED_MODEL_ID.to_owned()),
        auto_download: false,
        active_profile: None,
        rollback_profile: None,
        resources: SemanticResourceCeilings::default(),
    };
    assert!(
        super::semantic_retrieval_profiles_disabled(&disabled),
        "no committed retrieval profile is the genuine Plan 20 default-off state"
    );

    let active = SemanticConfig {
        active_profile: Some(selection.clone()),
        ..disabled.clone()
    };
    assert!(
        !super::semantic_retrieval_profiles_disabled(&active),
        "a committed active profile expects a seated coordinator: stay retryable"
    );

    let rollback_only = SemanticConfig {
        rollback_profile: Some(selection),
        ..disabled
    };
    assert!(
        !super::semantic_retrieval_profiles_disabled(&rollback_only),
        "a committed rollback profile still pins vector machinery: stay retryable"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unseated_semantic_runtime_sweeps_quietly_without_a_degraded_loop() {
    let fixture = open_unseated_graph_fixture().await;
    let root = fixture.graph.project_root();

    // Two full passes: the default-off state must stay a quiet success on
    // every pass, never the vector_census_incomplete degraded retry loop.
    for pass in 0..2_usize {
        assert!(
            run_semantic_vector_generation_retention(
                &fixture.graph,
                &fixture.schedulers,
                &fixture.observations,
                &fixture.cancellation,
            )
            .await
            .is_complete(),
            "pass {pass}: an unseated semantic runtime is an ordinary success, not a failure"
        );
        assert_eq!(
            fixture.observations.semantic_vector_retention_read(root),
            SemanticVectorRetentionReadV1::SemanticUnseated,
            "pass {pass}: the census read must pin the typed unseated state"
        );
        let inventory = resolve_vector_retention_inventory(
            &fixture.graph,
            &fixture.schedulers,
            &fixture.observations,
        )
        .await;
        assert!(
            matches!(inventory, VectorRetentionInventoryV1::SemanticUnseated),
            "pass {pass}: the retention inventory must be the quiet unseated pin"
        );
        assert_eq!(
            inventory.degraded_reason(),
            None,
            "pass {pass}: default-off semantic must not report vector_inventory_offline"
        );
        assert_eq!(
            run_code_generation_retention(
                &fixture.graph,
                &fixture.schedulers,
                &fixture.observations,
                &fixture.cancellation,
            )
            .await,
            CodeGenerationRetentionOutcomeV1::MoreWork,
            "pass {pass}: the unseated offline sweep succeeds quietly and reports its backlog"
        );
    }

    // The quiet pin still sweeps: one bounded superseded generation per pass
    // is collected under the offline protection set, and the active head
    // survives. Semantic-off profiles must not grow without bound.
    let remaining = sealed_generation_files(&fixture.store_root);
    assert_eq!(
        remaining.len(),
        FIXTURE_GENERATION_COUNT - 2,
        "each pass collects exactly one bounded superseded generation"
    );
    assert!(
        remaining.contains(&active_generation_file(&fixture.store_root)),
        "the active publication head is never collected"
    );

    // The full per-project maintenance unit drains the remaining backlog on
    // the bounded code-generation continuation — one collection unit per
    // tick — and converges to a success once a census proves the store
    // holds only the active head. That convergence is what returns the
    // cadence to its ordinary interval instead of the short retry loop.
    let mut continuation = None;
    let mut ticks = 0_usize;
    loop {
        ticks += 1;
        assert!(
            ticks <= FIXTURE_GENERATION_COUNT + 2,
            "the generation-maintenance unit must converge instead of continuing forever"
        );
        let outcome = crate::daemon::maintenance::generation::run_project_generation_maintenance(
            &fixture.graph,
            &fixture.schedulers,
            &fixture.observations,
            &fixture.cancellation,
            &crate::config::RetentionConfig::default(),
            continuation,
        )
        .await;
        match outcome {
            crate::daemon::maintenance::MaintenanceTickOutcome::Complete => break,
            crate::daemon::maintenance::MaintenanceTickOutcome::Continue(
                crate::daemon::maintenance::MaintenanceContinuation::CodeGenerationRetention,
            ) => {
                continuation = Some(
                    crate::daemon::maintenance::MaintenanceContinuation::CodeGenerationRetention,
                );
            }
            other => panic!("unexpected generation-maintenance outcome: {other:?}"),
        }
    }
    let converged = sealed_generation_files(&fixture.store_root);
    assert_eq!(
        converged.len(),
        1,
        "the continuation cadence drains every superseded generation"
    );
    assert!(
        converged.contains(&active_generation_file(&fixture.store_root)),
        "only the active publication head survives the drain"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_vector_continuation_skips_code_generation_retention() {
    let fixture = open_unseated_graph_fixture().await;
    let before = sealed_generation_files(&fixture.store_root);

    let outcome = crate::daemon::maintenance::generation::run_project_generation_maintenance(
        &fixture.graph,
        &fixture.schedulers,
        &fixture.observations,
        &fixture.cancellation,
        &crate::config::RetentionConfig::default(),
        Some(crate::daemon::maintenance::MaintenanceContinuation::SemanticVectorRetention),
    )
    .await;

    assert!(
        outcome.is_complete(),
        "the quiet unseated semantic phase completes without escalating to a retry"
    );
    assert_eq!(
        sealed_generation_files(&fixture.store_root),
        before,
        "a semantic-vector continuation must not re-run code-generation retention"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scanning_census_defers_the_sweep_without_the_degraded_reason() {
    let fixture = open_unseated_graph_fixture().await;
    record_paging_census(&fixture.observations, fixture.graph.project_root());

    let inventory = resolve_vector_retention_inventory(
        &fixture.graph,
        &fixture.schedulers,
        &fixture.observations,
    )
    .await;
    assert!(
        matches!(inventory, VectorRetentionInventoryV1::CensusScanning),
        "a paging census resolves to the in-progress inventory, not offline"
    );
    assert_eq!(
        inventory.degraded_reason(),
        None,
        "census paging must not log vector_inventory_offline:vector_census_incomplete"
    );

    assert_eq!(
        run_code_generation_retention(
            &fixture.graph,
            &fixture.schedulers,
            &fixture.observations,
            &fixture.cancellation,
        )
        .await,
        CodeGenerationRetentionOutcomeV1::Complete,
        "a paging census defers the sweep instead of degrading"
    );
    assert_eq!(
        sealed_generation_files(&fixture.store_root).len(),
        FIXTURE_GENERATION_COUNT,
        "no generation is collected while the exact pin set is still paging"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_census_still_degrades_to_the_offline_inventory() {
    let fixture = open_unseated_graph_fixture().await;

    // Unknown (no progress recorded at all) stays a reported degradation:
    // a seated runtime whose census was reset by a failure or mutation.
    let inventory = resolve_vector_retention_inventory(
        &fixture.graph,
        &fixture.schedulers,
        &fixture.observations,
    )
    .await;
    let VectorRetentionInventoryV1::Offline { reason } = &inventory else {
        panic!("an unknown census must resolve to the degraded offline inventory");
    };
    assert_eq!(reason, "vector_census_incomplete");
    assert_eq!(
        inventory.degraded_reason().as_deref(),
        Some("vector_inventory_offline:vector_census_incomplete"),
    );

    // The degraded offline pass still sweeps under the offline protection
    // set so sealed files cannot grow without bound while the graph is dark.
    assert_eq!(
        run_code_generation_retention(
            &fixture.graph,
            &fixture.schedulers,
            &fixture.observations,
            &fixture.cancellation,
        )
        .await,
        CodeGenerationRetentionOutcomeV1::MoreWork,
    );
    assert_eq!(
        sealed_generation_files(&fixture.store_root).len(),
        FIXTURE_GENERATION_COUNT - 1,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_corrupt_and_denied_vector_authorities_refuse_the_sweep() {
    let fixture = open_unseated_graph_fixture().await;
    let global_db =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open("vector-refusals")
            .await;
    let scope = tracedecay_application::ResolvedScope::new(
        tracedecay_domain::ProjectId::new("project.retention-fixture").expect("project id"),
        tracedecay_domain::RepositoryId::new("repository.retention-fixture")
            .expect("repository id"),
        tracedecay_domain::WorktreeId::new("worktree.retention-fixture").expect("worktree id"),
        None,
    )
    .expect("resolved scope");
    let (_, revision) = fixture_census_shard();

    for (sources, expected_reason) in [
        (
            ProjectVectorReadableSources::ResetRequired("stage journal replay".to_owned()),
            "vector_graph_reset_required:stage journal replay",
        ),
        (
            ProjectVectorReadableSources::Corrupt("torn census record".to_owned()),
            "vector_graph_corrupt:torn census record",
        ),
        (
            ProjectVectorReadableSources::Denied("scope mismatch".to_owned()),
            "vector_graph_denied:scope mismatch",
        ),
    ] {
        let configuration =
            tracedecay_usecases::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1::open(
                global_db.registered.clone(),
                scope.clone(),
            )
            .expect("configuration inventory authority");
        let inventory = classify_vector_readable_sources(sources, configuration, revision);
        let VectorRetentionInventoryV1::Refused { reason } = &inventory else {
            panic!("{expected_reason}: reset/corrupt/denied must classify as Refused");
        };
        assert_eq!(reason, expected_reason);
        assert_eq!(
            inventory.degraded_reason().as_deref(),
            Some(expected_reason),
            "a refusal is always reported"
        );
        assert_eq!(
            apply_code_generation_retention(
                &fixture.graph,
                &fixture.schedulers,
                &fixture.observations,
                inventory,
                &fixture.cancellation,
            )
            .await,
            CodeGenerationRetentionOutcomeV1::Failed,
            "{expected_reason}: a refused inventory fails the pass"
        );
        assert_eq!(
            sealed_generation_files(&fixture.store_root).len(),
            FIXTURE_GENERATION_COUNT,
            "{expected_reason}: a refused inventory collects nothing"
        );
    }

    // An unavailable graph stays a degraded offline sweep, not a refusal.
    let configuration =
        tracedecay_usecases::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1::open(
            global_db.registered.clone(),
            scope,
        )
        .expect("configuration inventory authority");
    let inventory = classify_vector_readable_sources(
        ProjectVectorReadableSources::Unavailable("graph capacity saturated".to_owned()),
        configuration,
        revision,
    );
    let VectorRetentionInventoryV1::Offline { reason } = &inventory else {
        panic!("an unavailable graph must classify as the degraded offline inventory");
    };
    assert_eq!(reason, "vector_graph_unavailable:graph capacity saturated");
}

fn release_queue_files(store_root: &Path) -> usize {
    match std::fs::read_dir(store_root.join("graph-replay-release-queue")) {
        Ok(entries) => entries
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("release-"))
            })
            .count(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("release queue directory is unreadable: {error}"),
    }
}

/// A held replay pool defers the whole pass in one non-blocking probe — no
/// full-digest planning, no lock spinning, no deadline-free flock behind the
/// daemon writer gate. The following tick collects but skips the release
/// reconcile under the backoff window, and the tick after that drains the
/// accumulated queue. This is the wedge journey from the live incident —
/// `graph_replay_release_failed error=DeadlineExceeded` on every tick while a
/// publisher held the pool — reduced to one loud deferral, one quiet skip,
/// and a full recovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_replay_pool_defers_then_backs_off_then_recovers() {
    let fixture = open_unseated_graph_fixture().await;
    let project_root = fixture.graph.project_root().to_path_buf();
    let replay_root = fixture
        .graph
        .db()
        .database_path()
        .with_extension("graph-replay");
    tracedecay_runtime_core::storage::PrivateStoreIo::create_private_directory(&replay_root)
        .expect("create owner-private replay pool");
    let held_pool = try_acquire_code_generation_store_lock(&replay_root)
        .expect("probe the replay pool lock")
        .expect("acquire the replay pool lock");

    // Pass A: the probe sees the held pool and defers the pass before any
    // collection or release work starts; nothing blocks on the holder.
    assert_eq!(
        run_code_generation_retention(
            &fixture.graph,
            &fixture.schedulers,
            &fixture.observations,
            &fixture.cancellation,
        )
        .await,
        CodeGenerationRetentionOutcomeV1::Failed,
        "a held replay pool defers the pass without blocking on the holder"
    );
    assert_eq!(
        sealed_generation_files(&fixture.store_root).len(),
        FIXTURE_GENERATION_COUNT,
        "nothing is collected while the pool is held"
    );
    assert_eq!(
        release_queue_files(&fixture.store_root),
        0,
        "no release evidence is queued while the pool is held"
    );

    drop(held_pool);

    // Pass B: the pool is free again, so collection makes bounded progress,
    // but the backoff window from the busy attempt skips this tick's release
    // reconcile — no lock traffic, no graph probe — leaving the fresh release
    // evidence durably queued.
    assert_eq!(
        run_code_generation_retention(
            &fixture.graph,
            &fixture.schedulers,
            &fixture.observations,
            &fixture.cancellation,
        )
        .await,
        CodeGenerationRetentionOutcomeV1::Failed,
        "the backoff window defers the reconcile for one tick after the busy attempt"
    );
    assert_eq!(
        sealed_generation_files(&fixture.store_root).len(),
        FIXTURE_GENERATION_COUNT - 1,
        "collection resumes as soon as the pool frees"
    );
    assert_eq!(
        release_queue_files(&fixture.store_root),
        1,
        "the deferred tick's release evidence stays durably queued"
    );

    // Pass C: the window is spent, the reconcile drains the queued release
    // plus this tick's own, and the pass reports its remaining collectable
    // backlog as bounded progress.
    assert_eq!(
        run_code_generation_retention(
            &fixture.graph,
            &fixture.schedulers,
            &fixture.observations,
            &fixture.cancellation,
        )
        .await,
        CodeGenerationRetentionOutcomeV1::MoreWork,
        "a recovered pool drains the queue and reports the remaining backlog"
    );
    assert_eq!(
        release_queue_files(&fixture.store_root),
        0,
        "recovery consumes every queued release event"
    );
    assert!(
        fixture
            .observations
            .graph_replay_release_attempt_admitted(&project_root),
        "a served reconcile closes the backoff window"
    );
}

/// The probe-to-execute TOCTOU at the executor boundary: the outer probe
/// would have succeeded, the publisher then holds the pool, and collection
/// must return the typed busy result and drop the daemon writer gate inside
/// the acquire budget instead of parking on a blocking flock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publisher_between_probe_and_execute_releases_writer_gate_promptly() {
    let fixture = open_unseated_graph_fixture().await;
    let replay_root = fixture
        .graph
        .db()
        .database_path()
        .with_extension("graph-replay");
    tracedecay_runtime_core::storage::PrivateStoreIo::create_private_directory(&replay_root)
        .expect("create owner-private replay pool");

    let probe = try_acquire_code_generation_store_lock(&replay_root)
        .expect("outer probe")
        .expect("probe finds a free pool");
    drop(probe);

    let publisher = try_acquire_code_generation_store_lock(&replay_root)
        .expect("publisher probe")
        .expect("publisher wins the probe-to-execute window");
    let plan = plan_code_generation_retention(
        &fixture.store_root,
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("plan collectable retention");
    assert!(
        !plan.collectable_generations.is_empty(),
        "execute only acquires the pool when a generation is collectable"
    );

    let gates = StoreWriterGates::default();
    let started = Instant::now();
    let error = {
        let writer = gates
            .try_acquire(&WriterScope::Daemon)
            .expect("maintenance holds the daemon writer gate");
        let error = execute_code_generation_retention(
            &fixture.store_root,
            plan,
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(130),
            Some(&replay_root),
        )
        .expect_err("held pool after a successful probe must defer");
        drop(writer);
        error
    };
    let elapsed = started.elapsed();

    assert!(
        matches!(error, CodeGenerationRetentionErrorV1::GraphReplayPoolBusy),
        "executor must return the typed busy result, got {error:?}"
    );
    assert!(
        elapsed < GRAPH_REPLAY_POOL_ACQUIRE_BUDGET + Duration::from_millis(50),
        "writer-held acquire must stay inside the budget, took {elapsed:?}"
    );
    assert!(
        gates.try_acquire(&WriterScope::Daemon).is_some(),
        "the daemon writer gate must be free as soon as execute defers"
    );
    assert_eq!(
        sealed_generation_files(&fixture.store_root).len(),
        FIXTURE_GENERATION_COUNT,
        "a deferred execute must not collect"
    );

    drop(publisher);
}

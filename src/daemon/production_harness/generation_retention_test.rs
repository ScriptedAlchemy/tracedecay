use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::doctor::{
    DoctorEvidenceStateV1, DoctorStorageFamilyReadV1, DoctorStorageFindingKindV1,
    DoctorStorageIncompleteReasonV1,
};
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision,
    CodeGenerationId, CodeSearchChunkId, ContentDigest, EmbeddingDeviceClassV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
    EmbeddingTruncationSideV1, PrivacyDomainId, ProjectionBatchRequestV1, ProjectionReplayReasonV1,
};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_semantic::projector::{PreparedVectorGenerationV1, ProjectedChunkVectorV1};

use super::journey_test_support::git;
use super::*;
use crate::retention::code_index_generations::{
    DEFAULT_SUPERSEDED_GENERATION_FLOOR, prepare_next_code_generation_retention_cancellable,
};
use crate::store::vector_generations::{
    GraphVectorGenerationStoreV1, SemanticVectorStageDescriptorV1, VectorGenerationPlanV1,
};
use tracedecay_usecases::semantic_runtime::project_semantic_retained_vector_generations;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn digest<T>(byte: char) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn initialize_git_project(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "TraceDecay Test"]);
    git(
        root,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::create_dir_all(root.join("src")).expect("source directory");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn retained() -> usize { 0 }\n",
    )
    .expect("initial source");
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "initial"]);
}

fn admitted_embedding() -> AdmittedEmbeddingProjectionKeyV1 {
    EmbeddingProjectionKeyV1 {
        model_artifact_digest: digest('1'),
        tokenizer_digest: digest('2'),
        config_digest: digest('3'),
        query_instruction_digest: None,
        document_instruction_digest: None,
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 512,
        inference_batch_size: 8,
        inference_batch_bytes: 16 * 1024,
        runtime_backend: "fastembed-ort".to_owned(),
        runtime_build_revision: "runtime.maintenance-retention.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 2,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_owned(),
        chunker_revision: id::<ChunkerRevision>("chunker.maintenance-retention.v1"),
        privacy_domain: id::<PrivacyDomainId>("privacy.maintenance-retention"),
        privacy_key_epoch: 1,
    }
    .admit()
    .expect("admitted embedding")
}

fn prepared_vector(source: &CodeGenerationId) -> PreparedVectorGenerationV1 {
    let embedding_key = admitted_embedding();
    let projection_key = embedding_key.projection_key().clone();
    let chunk_id = id::<CodeSearchChunkId>("chunk.maintenance-retention");
    let chunk_digest = digest::<ContentDigest>('a');
    let values = vec![1.0, 0.0];
    let output_digest = tracedecay_semantic::projector::vector_output_digest(
        &projection_key,
        &chunk_id,
        &chunk_digest,
        &values,
    )
    .expect("vector output digest");
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: None,
        to_generation: source.clone(),
        manifest_digest: digest('0'),
        added_or_changed: vec![ChangedCodeChunkV1 {
            chunk_id: chunk_id.clone(),
            prior_digest: None,
            current_digest: Some(chunk_digest.clone()),
        }],
        deleted: Vec::new(),
        reused: Vec::new(),
    };
    changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
    let source_manifest_digest = changes.manifest_digest.clone();
    let mut request = ProjectionBatchRequestV1 {
        request_digest: digest('0'),
        changes,
        previous_projection_key: None,
        target_projection_key: projection_key.clone(),
        replay_reason: ProjectionReplayReasonV1::SourceEdit,
    };
    request.request_digest = tracedecay_code_index::projection::expected_request_digest(&request)
        .expect("projection request digest");
    let receipt = tracedecay_code_index::projection::build_batch_receipt(
        &request,
        &[
            tracedecay_code_index::projection::ChunkProjectionDecisionV1 {
                chunk_id: chunk_id.clone(),
                prior_chunk_digest: None,
                current_chunk_digest: Some(chunk_digest.clone()),
                operation: tracedecay_domain::ProjectionOperationV1::Added,
                outcome: tracedecay_domain::ProjectionOutcomeV1::Applied,
                output_digest: Some(output_digest.clone()),
            },
        ],
    )
    .expect("projection receipt");
    PreparedVectorGenerationV1 {
        embedding_key,
        request,
        receipt,
        vectors: vec![ProjectedChunkVectorV1 {
            projection_key,
            source_generation: source.clone(),
            source_manifest_digest,
            chunk_id,
            chunk_digest,
            values,
            output_digest,
        }],
        tombstones: Vec::new(),
    }
}

async fn publish_vector_generation(
    schedulers: &crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    source: &CodeGenerationId,
) -> tracedecay_domain::VectorGenerationIdV1 {
    let provider = schedulers
        .semantic_vector_graph_provider(project_root)
        .await
        .expect("mounted semantic vector provider");
    let retained = provider
        .graph_for_current()
        .await
        .expect("retained current persistent vector graph");
    let store =
        GraphVectorGenerationStoreV1::open(&retained).expect("open vector generation store");
    let prepared = prepared_vector(source);
    store
        .configure_stage(
            SemanticVectorStageDescriptorV1::from_changes(
                prepared.embedding_key.clone(),
                &prepared.request.changes,
            )
            .expect("semantic stage descriptor"),
        )
        .expect("configure semantic stage");
    let chunk_ids = prepared
        .request
        .changes
        .added_or_changed
        .iter()
        .map(|change| change.chunk_id.clone())
        .collect::<Vec<_>>();
    let plan = VectorGenerationPlanV1 {
        target_projection_key: prepared.embedding_key.projection_key().clone(),
        source_generation: source.clone(),
        source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
        expected_chunk_ids: chunk_ids.into(),
        base_generation: None,
    };
    let begin = store
        .begin_generation(plan, Arc::new(NeverCancelled))
        .await
        .expect("begin semantic vector generation");
    store
        .commit_batch(begin.build_id(), None, prepared, Arc::new(NeverCancelled))
        .await
        .expect("commit semantic vector batch");
    let publication = store
        .publish_generation(begin.build_id(), Arc::new(NeverCancelled))
        .await
        .expect("publish semantic vector generation");
    publication.generation_id
}

async fn wait_for_changed_generation(
    schedulers: &crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    prior: &CodeGenerationId,
) -> CodeGenerationId {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(current) = schedulers.latest_generation_id(project_root).await
                && &current != prior
            {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("changed code generation")
}

async fn publish_code_edit(
    schedulers: &crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    prior: &CodeGenerationId,
    revision: usize,
) -> CodeGenerationId {
    std::fs::write(
        project_root.join("src/lib.rs"),
        format!("pub fn retained() -> usize {{ {revision} }}\n"),
    )
    .expect("edit source");
    assert!(
        schedulers
            .notify_hook_paths(project_root, &["src/lib.rs".to_owned()])
            .await,
        "mounted scheduler accepts the exact worktree hint"
    );
    wait_for_changed_generation(schedulers, project_root, prior).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mounted_daemon_maintenance_retains_activation_lease_and_converges_after_restart() {
    let isolation = TempDir::new().expect("isolated production composition");
    let project_root = isolation.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    initialize_git_project(&project_root);

    let harness =
        ProductionProjectCompositionHarnessV1::open(isolation.path(), [project_root.clone()])
            .await
            .expect("mounted production composition");
    let resources = harness.resources.as_ref().expect("live harness resources");
    let schedulers = &resources.invocation.code_index_schedulers;
    let graph = harness
        .server(&project_root)
        .expect("project server")
        .cg()
        .await;
    let canonical_root = graph.project_root().to_path_buf();
    let first_source = schedulers
        .latest_generation_id(&canonical_root)
        .await
        .expect("initial sealed code generation");

    assert!(
        project_semantic_retained_vector_generations(&canonical_root)
            .is_some_and(|roots| roots.generation_ids().is_empty()),
        "the exact committed query-only profile is known-empty retention authority"
    );
    let vector_generation =
        publish_vector_generation(schedulers, &canonical_root, &first_source).await;
    let provider = schedulers
        .semantic_vector_graph_provider(&canonical_root)
        .await
        .expect("mounted vector provider");
    let retained = provider
        .graph_for_current()
        .await
        .expect("persistent vector graph");
    let activation_lease =
        GraphVectorGenerationStoreV1::read_only_generation(&retained, &vector_generation)
            .expect("read exact activation generation")
            .expect("published activation generation");
    drop(retained);

    let mut latest = first_source.clone();
    for revision in 1..=4 {
        latest = publish_code_edit(schedulers, &canonical_root, &latest, revision).await;
    }
    let newer_vector_generation =
        publish_vector_generation(schedulers, &canonical_root, &latest).await;
    assert_ne!(newer_vector_generation, vector_generation);
    let code_store_root = crate::daemon::code_index_scheduler::scoped_code_index_store_root(
        &graph.store_layout().data_root.join("code-index-v1"),
        &canonical_root,
    );
    let graph_replay_pool_root = graph.db().database_path().with_extension("graph-replay");
    let plan = prepare_next_code_generation_retention_cancellable(
        &code_store_root,
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
        Some(&graph_replay_pool_root),
    )
    .expect("code generation retention plan");
    let first_candidate = plan
        .collectable_generations
        .iter()
        .find(|generation| generation.generation_id == first_source)
        .expect("vector source is old enough to collect");
    let first_source_file = code_store_root
        .join("code-generations-v1")
        .join(&first_candidate.generation_file);
    assert!(first_source_file.is_file());

    let observations = resources.store_administration.store_telemetry_sampling();
    let cancellation = tracedecay_usecases::context::CancellationToken::new();
    assert!(
        !crate::daemon::maintenance::generation::run_project_generation_maintenance(
            graph.as_ref(),
            schedulers,
            &observations,
            &cancellation,
            &crate::config::RetentionConfig::default(),
        )
        .await,
        "the bounded census defers code retention while a later vector remains"
    );
    assert!(
        first_source_file.is_file(),
        "code deletion must not race ahead of the retained vector source"
    );
    assert!(
        crate::daemon::maintenance::generation::run_project_generation_maintenance(
            graph.as_ref(),
            schedulers,
            &observations,
            &cancellation,
            &crate::config::RetentionConfig::default(),
        )
        .await,
        "the retained activation lease remains a successful non-mutating observation"
    );
    assert!(
        first_source_file.is_file(),
        "exact vector-source liveness must veto the source-code deletion plan"
    );
    let observed_inventory = crate::daemon::store_maintenance::resolve_vector_retention_inventory(
        graph.as_ref(),
        schedulers,
        &observations,
    )
    .await;
    assert!(
        matches!(
            observed_inventory,
            crate::daemon::store_maintenance::VectorRetentionInventoryV1::Online { .. }
        ),
        "a complete post-convergence census pins through the online vector inventory"
    );
    assert_eq!(
        observed_inventory.degraded_reason(),
        None,
        "the online inventory reports no degradation"
    );
    assert!(matches!(
        crate::daemon::doctor_kernel::collect_code_generation_retention_findings(
            schedulers,
            &observations,
            graph
                .configuration_runtime()
                .semantic_configuration_inventory_authority()
                .as_ref(),
            &code_store_root,
            &canonical_root,
        )
        .await,
        DoctorStorageFamilyReadV1::ObservedIncomplete { .. }
    ));

    drop(activation_lease);
    assert!(
        !crate::daemon::maintenance::generation::run_project_generation_maintenance(
            graph.as_ref(),
            schedulers,
            &observations,
            &cancellation,
            &crate::config::RetentionConfig::default(),
        )
        .await,
        "the vector retirement action defers source-code collection"
    );
    assert!(first_source_file.is_file());
    drop(graph);
    harness.shutdown().await;

    let restarted =
        ProductionProjectCompositionHarnessV1::open(isolation.path(), [project_root.clone()])
            .await
            .expect("restart mounted production composition");
    let restarted_resources = restarted
        .resources
        .as_ref()
        .expect("restarted harness resources");
    let restarted_schedulers = &restarted_resources.invocation.code_index_schedulers;
    let restarted_graph = restarted
        .server(&project_root)
        .expect("restarted project server")
        .cg()
        .await;
    let restarted_observations = restarted_resources
        .store_administration
        .store_telemetry_sampling();
    let restarted_cancellation = tracedecay_usecases::context::CancellationToken::new();
    let mut converged = false;
    for _ in 0..4 {
        converged = crate::daemon::maintenance::generation::run_project_generation_maintenance(
            restarted_graph.as_ref(),
            restarted_schedulers,
            &restarted_observations,
            &restarted_cancellation,
            &crate::config::RetentionConfig::default(),
        )
        .await;
        if converged {
            break;
        }
        assert!(
            first_source_file.is_file(),
            "cleanup convergence must finish before source-code deletion"
        );
    }
    assert!(converged, "replayed cleanup converges after restart");
    assert!(
        !first_source_file.exists(),
        "the code source becomes collectible only after vector cleanup"
    );
    let doctor = crate::daemon::doctor_kernel::collect_code_generation_retention_findings(
        restarted_schedulers,
        &restarted_observations,
        restarted_graph
            .configuration_runtime()
            .semantic_configuration_inventory_authority()
            .as_ref(),
        &code_store_root,
        &canonical_root,
    )
    .await;
    let DoctorStorageFamilyReadV1::ObservedIncomplete { findings, reason } = doctor else {
        panic!("surviving nonconfigured graph head must be reported as incomplete: {doctor:?}");
    };
    assert_eq!(reason, DoctorStorageIncompleteReasonV1::Unknown);
    assert_eq!(findings.len(), 2);
    assert_eq!(
        findings[0].kind(),
        DoctorStorageFindingKindV1::RetentionBacklog
    );
    assert_eq!(
        findings[0].finding().state(),
        DoctorEvidenceStateV1::HealthyCompleteCoverage,
        "the surviving head is live, not stale cleanup backlog"
    );
    assert!(
        findings[0]
            .finding()
            .evidence()
            .iter()
            .any(|evidence| evidence
                .reference()
                .as_str()
                .contains("nonconfigured-published-1")),
        "Doctor names the one still-live nonconfigured head"
    );
    assert_eq!(
        findings[1].kind(),
        DoctorStorageFindingKindV1::RetentionBacklog
    );
    assert_eq!(
        findings[1].finding().state(),
        DoctorEvidenceStateV1::Partial,
        "code retention remains partial while exact vector-head collectability is unknown"
    );
    restarted.shutdown().await;
}

#[cfg(feature = "semantic-fastembed")]
async fn set_semantic_disabled(harness: &ProductionProjectCompositionHarnessV1, project: &Path) {
    let graph = harness.server(project).expect("project server").cg().await;
    let expected_revision = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("current production configuration")
        .revision_id;
    let request = tracedecay_application::ConfigurationSetRequestV1 {
        layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Default,
        key: tracedecay_domain::configuration::SettingKey::new(
            crate::config::SEMANTIC_RUNTIME_SETTING_KEY,
        )
        .expect("semantic runtime setting key"),
        value: tracedecay_domain::configuration::ConfigurationValueV1::Text(
            serde_json::to_string(&crate::config::SemanticConfig {
                selected_model: Some(crate::semantic_code::DEFAULT_FASTEMBED_MODEL_ID.to_owned()),
                auto_download: false,
                active_profile: None,
                rollback_profile: None,
                resources: crate::config::SemanticResourceCeilings::default(),
            })
            .expect("disabled semantic runtime JSON"),
        ),
        idempotency_key: tracedecay_domain::configuration::ConfigurationIdempotencyKey::new(
            format!("configuration.idempotency.semantic-retention-disable.{expected_revision}"),
        )
        .expect("semantic disable idempotency key"),
        expected_revision,
    };
    let response = harness
        .call_tool(
            project,
            "tracedecay_configuration_set",
            serde_json::to_value(request).expect("configuration set request"),
        )
        .await
        .expect("public semantic configuration disable");
    assert!(
        response.error.is_none(),
        "configuration disable failed: {response:?}"
    );
    let result = response
        .result
        .as_ref()
        .expect("configuration disable result");
    assert_ne!(
        result["isError"], true,
        "configuration disable failed: {result}"
    );
}

#[cfg(feature = "semantic-fastembed")]
async fn vector_generation_exists(
    schedulers: &crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    generation: &tracedecay_domain::VectorGenerationIdV1,
) -> bool {
    let provider = schedulers
        .semantic_vector_graph_provider(project_root)
        .await
        .expect("mounted semantic vector provider");
    let retained = provider
        .graph_for_current()
        .await
        .expect("current semantic vector graph");
    GraphVectorGenerationStoreV1::read_only_generation(&retained, generation)
        .expect("read exact semantic vector generation")
        .is_some()
}

#[cfg(feature = "semantic-fastembed")]
async fn run_generation_cadence(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
) -> bool {
    let resources = harness.resources.as_ref().expect("live harness resources");
    let graph = harness
        .server(project_root)
        .expect("project server")
        .cg()
        .await;
    crate::daemon::maintenance::generation::run_project_generation_maintenance(
        graph.as_ref(),
        &resources.invocation.code_index_schedulers,
        &resources.store_administration.store_telemetry_sampling(),
        &tracedecay_usecases::context::CancellationToken::new(),
        &crate::config::RetentionConfig::default(),
    )
    .await
}

#[cfg(feature = "semantic-fastembed")]
const EIGHT_DAYS_SECS: i64 = 8 * 24 * 60 * 60;

#[cfg(feature = "semantic-fastembed")]
fn age_scope_for_reconciliation(scope: &Path) {
    let old = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(EIGHT_DAYS_SECS as u64))
        .expect("scope age timestamp");
    let mtime = filetime::FileTime::from_system_time(old);
    let mut pending = vec![scope.to_path_buf()];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            for entry in std::fs::read_dir(&path).expect("walk scope for aging") {
                pending.push(entry.expect("scope age entry").path());
            }
        }
        filetime::set_file_mtime(&path, mtime).expect("age scope entry");
    }
}

#[cfg(feature = "semantic-fastembed")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_worktree_scope_retention_crash_replay_and_pure_inventory_journey() {
    use super::semantic_activation_journey_test::{
        evaluate_native_profile, installed_selection_material, seed_distribution_fixture,
        selection, semantic_candidate, set_semantic_profile, wait_for_semantic_generation,
    };

    let fixture_root = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_dir())
        .expect("linked-worktree retention journey requires the distribution FastEmbed fixture");
    let _profile = crate::config::PinnedUserDataDir::new();
    let lifecycle_root =
        crate::semantic_code::default_lifecycle_root().expect("isolated lifecycle root");
    let lifecycle =
        crate::semantic_code::shared_lifecycle_owner().expect("production lifecycle owner");
    seed_distribution_fixture(&lifecycle_root, &fixture_root, &lifecycle);
    lifecycle
        .select_model(Some(crate::semantic_code::DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("select production semantic model");
    lifecycle
        .acquire_blocking_for_tests()
        .expect("install verified distribution fixture");
    let (artifact_digest, artifact_path) = installed_selection_material(&lifecycle);

    let isolation = TempDir::new().expect("linked-worktree journey isolation");
    let primary = isolation.path().join("primary");
    std::fs::create_dir_all(&primary).expect("primary worktree");
    initialize_git_project(&primary);
    let linked = isolation.path().join("linked-b");
    let linked_arg = linked.to_string_lossy().into_owned();
    git(
        &primary,
        &["worktree", "add", "-q", "-b", "linked-b", &linked_arg],
    );
    std::fs::write(
        linked.join("src/lib.rs"),
        "pub fn retained() -> usize { 101 }\n",
    )
    .expect("linked-worktree source");
    git(&linked, &["add", "."]);
    git(&linked, &["commit", "-qm", "linked semantic source"]);

    let harness = ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [primary.clone(), linked.clone()],
    )
    .await
    .expect("mounted linked-worktree production composition");
    let resources = harness.resources.as_ref().expect("live harness resources");
    let primary_code_id = resources
        .invocation
        .code_index_schedulers
        .latest_generation_id(&primary)
        .await
        .expect("primary code generation");
    let linked_code_id = resources
        .invocation
        .code_index_schedulers
        .latest_generation_id(&linked)
        .await
        .expect("linked code generation");
    assert_ne!(primary_code_id, linked_code_id);
    let (primary_code, primary_vector) =
        wait_for_semantic_generation(&harness, &primary, &primary_code_id).await;
    let (linked_code, linked_vector) =
        wait_for_semantic_generation(&harness, &linked, &linked_code_id).await;
    assert_ne!(
        primary_vector.generation_id(),
        linked_vector.generation_id()
    );
    let primary_profile = evaluate_native_profile(
        &harness,
        &primary,
        semantic_candidate(&primary_code, &primary_vector),
    )
    .await;
    let linked_profile = evaluate_native_profile(
        &harness,
        &linked,
        semantic_candidate(&linked_code, &linked_vector),
    )
    .await;
    let primary_selection = selection(primary_profile, &artifact_digest, &artifact_path);
    let linked_selection = selection(linked_profile, &artifact_digest, &artifact_path);
    set_semantic_profile(
        &harness,
        &primary,
        primary_selection.clone(),
        Some(linked_selection.clone()),
    )
    .await;
    set_semantic_profile(
        &harness,
        &linked,
        linked_selection.clone(),
        Some(primary_selection.clone()),
    )
    .await;
    let primary_vector_id = primary_vector.generation_id().clone();
    let linked_vector_id = linked_vector.generation_id().clone();
    let primary_graph = harness.server(&primary).expect("primary server").cg().await;
    let linked_graph = harness.server(&linked).expect("linked server").cg().await;
    let linked_scope = crate::daemon::store_maintenance::code_index_scope_store_root(
        &primary_graph.hook_store_layout().data_root,
    )
    .join(
        crate::retention::code_index_generations::code_index_scope_hash(
            linked_graph.project_root(),
        ),
    );
    assert!(linked_scope.is_dir());
    drop(primary_graph);
    drop(linked_graph);
    drop(primary_code);
    drop(linked_code);
    drop(primary_vector);
    drop(linked_vector);
    harness.shutdown().await;

    let restarted =
        ProductionProjectCompositionHarnessV1::open(isolation.path(), [primary.clone()])
            .await
            .expect("A-only restart");
    let restarted_schedulers = &restarted
        .resources
        .as_ref()
        .expect("restarted resources")
        .invocation
        .code_index_schedulers;
    let mut complete = false;
    for _ in 0..8 {
        complete = run_generation_cadence(&restarted, &primary).await;
        assert!(vector_generation_exists(restarted_schedulers, &primary, &primary_vector_id).await);
        assert!(vector_generation_exists(restarted_schedulers, &primary, &linked_vector_id).await);
        assert!(linked_scope.is_dir(), "unmounted linked scope stays live");
        if complete {
            break;
        }
    }
    assert!(complete, "revision-pinned retained-root census converges");
    restarted.shutdown().await;

    let released = ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [primary.clone(), linked.clone()],
    )
    .await
    .expect("remount linked worktree for public release");
    let released_schedulers = &released
        .resources
        .as_ref()
        .expect("release resources")
        .invocation
        .code_index_schedulers;
    let newer_linked_code_id =
        publish_code_edit(released_schedulers, &linked, &linked_code_id, 202).await;
    let (newer_linked_code, newer_linked_vector) =
        wait_for_semantic_generation(&released, &linked, &newer_linked_code_id).await;
    assert_ne!(
        newer_linked_vector.generation_id(),
        &linked_vector_id,
        "released root has a newer projection head and is retirement-eligible"
    );
    let newest_linked_code_id =
        publish_code_edit(released_schedulers, &linked, &newer_linked_code_id, 203).await;
    let (newest_linked_code, newest_linked_vector) =
        wait_for_semantic_generation(&released, &linked, &newest_linked_code_id).await;
    assert!(
        tracedecay_usecases::semantic_runtime::project_semantic_retained_code_generation(
            &linked,
            &newer_linked_code_id,
        )
        .is_some(),
        "the intermediate unconfigured source starts process-retained"
    );
    let linked_graph = released.server(&linked).expect("linked server").cg().await;
    let linked_configuration = linked_graph
        .configuration_runtime()
        .semantic_configuration_inventory_authority()
        .expect("linked configuration inventory");
    let mut cursor = None;
    let linked_census = loop {
        let crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorRetentionStep::Ready(
            census,
        ) = crate::daemon::code_index_scheduler::semantic_vector_graph::retire_one_project_vector_generation(
            released_schedulers,
            &linked,
            &linked_configuration,
            cursor,
        )
        .await
        else {
            panic!("linked vector census must remain available");
        };
        if !matches!(
            census.action,
            tracedecay_graph_db::SemanticVectorRetentionAction::None
                | tracedecay_graph_db::SemanticVectorRetentionAction::Retained(_)
        ) {
            cursor = None;
            continue;
        }
        if let Some(receipt) = census.complete_receipt {
            break receipt;
        }
        cursor = census.continuation;
    };
    for _ in 0..2 {
        assert!(matches!(
            crate::daemon::code_index_scheduler::semantic_vector_graph::project_vector_readable_sources(
                released_schedulers,
                &linked,
                &linked_configuration,
                linked_census.revision,
            )
            .await,
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Ready {
                ..
            }
        ));
    }
    assert!(
        tracedecay_usecases::semantic_runtime::project_semantic_retained_code_generation(
            &linked,
            &newer_linked_code_id,
        )
        .is_some(),
        "two pure inventory reads must not prune an unconfigured non-latest source"
    );
    drop(linked_graph);
    let newer_linked_vector_id = newer_linked_vector.generation_id().clone();
    let newest_linked_vector_id = newest_linked_vector.generation_id().clone();
    drop(newer_linked_code);
    drop(newest_linked_code);
    drop(newer_linked_vector);
    drop(newest_linked_vector);
    let ((), _) = tokio::join!(
        set_semantic_profile(&released, &primary, primary_selection.clone(), None),
        run_generation_cadence(&released, &primary),
    );
    assert!(
        vector_generation_exists(released_schedulers, &primary, &primary_vector_id).await,
        "activation racing retention keeps the exact newly committed active root"
    );
    assert!(
        vector_generation_exists(released_schedulers, &primary, &linked_vector_id).await,
        "the other scope still pins its rollback root during activation"
    );
    set_semantic_disabled(&released, &linked).await;
    let mut retired = false;
    for _ in 0..12 {
        let _ = run_generation_cadence(&released, &primary).await;
        assert!(
            vector_generation_exists(released_schedulers, &primary, &primary_vector_id).await,
            "configured active root must survive every cadence"
        );
        if !vector_generation_exists(released_schedulers, &primary, &linked_vector_id).await
            && !vector_generation_exists(released_schedulers, &primary, &newer_linked_vector_id)
                .await
            && !vector_generation_exists(released_schedulers, &primary, &newest_linked_vector_id)
                .await
        {
            retired = true;
            break;
        }
    }
    assert!(
        retired,
        "only the publicly released linked-worktree root retires"
    );
    assert!(
        linked_scope.is_dir(),
        "registered linked scope remains intact"
    );
    released.shutdown().await;

    git(
        &primary,
        &["worktree", "remove", "--force", linked_arg.as_str()],
    );
    assert!(
        !linked.exists(),
        "the linked worktree is authentically removed"
    );
    age_scope_for_reconciliation(&linked_scope);

    let collector =
        ProductionProjectCompositionHarnessV1::open(isolation.path(), [primary.clone()])
            .await
            .expect("primary-only scope collection restart");
    let collector_schedulers = &collector
        .resources
        .as_ref()
        .expect("collector resources")
        .invocation
        .code_index_schedulers;
    let primary_scope = crate::daemon::store_maintenance::code_index_scope_store_root(
        &collector
            .server(&primary)
            .expect("collector primary server")
            .cg()
            .await
            .hook_store_layout()
            .data_root,
    )
    .join(crate::retention::code_index_generations::code_index_scope_hash(&primary));
    let _ = run_generation_cadence(&collector, &primary).await;
    assert!(
        !linked_scope.exists(),
        "released stranded scope is collected"
    );
    assert!(
        primary_scope.is_dir(),
        "configured primary scope survives collection"
    );
    assert!(
        vector_generation_exists(collector_schedulers, &primary, &primary_vector_id).await,
        "configured primary vector survives scope collection"
    );
    let cleanup_intent = linked_scope
        .parent()
        .expect("scope parent")
        .join(".code-index-scope-binding-cleanup-intent-v1.json");
    assert!(
        cleanup_intent.is_file(),
        "filesystem collection leaves the exact crash-replay cleanup intent"
    );
    collector.shutdown().await;

    let replayed = ProductionProjectCompositionHarnessV1::open(isolation.path(), [primary.clone()])
        .await
        .expect("scope cleanup replay restart");
    for _ in 0..4 {
        let _ = run_generation_cadence(&replayed, &primary).await;
        if !cleanup_intent.exists() {
            break;
        }
    }
    assert!(
        !cleanup_intent.exists(),
        "restart replays and completes the exact source-shard cleanup intent"
    );
    assert!(!linked_scope.exists());
    assert!(primary_scope.is_dir());
    assert!(
        vector_generation_exists(
            &replayed
                .resources
                .as_ref()
                .expect("replay resources")
                .invocation
                .code_index_schedulers,
            &primary,
            &primary_vector_id,
        )
        .await
    );
    replayed.shutdown().await;
}

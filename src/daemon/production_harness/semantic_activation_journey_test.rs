#![cfg(feature = "semantic-fastembed")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_application::ConfigurationSetRequestV1;
use tracedecay_domain::configuration::{ConfigurationLayerIdV1, ConfigurationValueV1, SettingKey};
use tracedecay_domain::{
    CalibrationProfileId, ComponentRevision, ManifestDigest, SemanticSearchIndexProfileV1,
    VectorGenerationIdV1, canonical_sha256,
};
use tracedecay_query::retrieval::semantic::SemanticCalibrationProfileV1;
use tracedecay_usecases::config::retrieval::{
    RetrievalCompatibilityPinsV1, SemanticCompatibilityPinsV1, SemanticResourceRequirementV1,
};
use tracedecay_usecases::semantic_runtime::{
    SemanticEvaluationDiversityCandidateV1, SemanticEvaluationFusionCandidateV1,
    SemanticEvaluationProfileCandidateV1, SemanticRuntimeStateV1,
};
use tracedecay_usecases::store::vector_generations::{
    GraphVectorGenerationStoreV1, PublishedVectorGenerationV1,
};

use super::*;

const EVALUATED_PROFILE_ID: &str = "hybrid-conservative";

pub(super) fn git(project: &Path, arguments: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(project)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output")
        .trim()
        .to_owned()
}

fn commit(project: &Path, message: &str) -> String {
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
    git(project, &["rev-parse", "HEAD"])
}

pub(super) fn tool_payload(response: &JsonRpcResponse) -> Value {
    assert!(response.error.is_none(), "tool failed: {response:?}");
    let result = response.result.as_ref().expect("tool result");
    let text = result["content"][0]["text"].as_str().expect("tool text");
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tool did not return JSON: {error}; result={result}; text={text}")
    })
}

fn assert_tool_effect_succeeded(response: &JsonRpcResponse) {
    assert!(response.error.is_none(), "tool failed: {response:?}");
    let result = response.result.as_ref().expect("tool result");
    assert_ne!(result["isError"], true, "tool effect failed: {result}");
}

pub(super) fn seed_distribution_fixture(
    lifecycle_root: &Path,
    fixture_root: &Path,
    owner: &crate::semantic_code::SemanticModelLifecycleOwnerV1,
) {
    let model = owner
        .catalog()
        .get(crate::semantic_code::DEFAULT_FASTEMBED_MODEL_ID)
        .expect("production catalog contains default model");
    let repository = format!("models--{}", model.model_code.replace('/', "--"));
    let repository_root = lifecycle_root.join("hf-hub-cache").join(repository);
    let snapshot = repository_root
        .join("snapshots")
        .join(&model.source.revision);
    for member in model.members.values() {
        let destination = snapshot.join(&member.upstream_path);
        std::fs::create_dir_all(destination.parent().expect("member parent"))
            .expect("create cached member parent");
        std::fs::copy(fixture_root.join(&member.path), &destination)
            .expect("copy byte-exact distribution fixture member");
    }
    let reference = repository_root.join("refs").join(&model.source.revision);
    std::fs::create_dir_all(reference.parent().expect("revision reference parent"))
        .expect("create revision reference parent");
    std::fs::write(reference, &model.source.revision).expect("write revision reference");
}

pub(super) fn installed_selection_material(
    owner: &crate::semantic_code::SemanticModelLifecycleOwnerV1,
) -> (String, PathBuf) {
    match owner.status().state.expect("installed model state") {
        crate::semantic_code::SemanticModelLifecycleStateV1::Installed {
            artifact_digest,
            install_path,
            ..
        }
        | crate::semantic_code::SemanticModelLifecycleStateV1::Ready {
            artifact_digest,
            install_path,
            ..
        } => (artifact_digest, install_path),
        state => panic!("expected installed production model, got {state:?}"),
    }
}

pub(super) async fn wait_for_semantic_generation(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    expected_source: &tracedecay_domain::CodeGenerationId,
) -> (
    Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>,
    PublishedVectorGenerationV1,
) {
    tokio::time::timeout(Duration::from_mins(3), async {
        loop {
            let resources = harness.resources.as_ref().expect("live harness");
            let Some(scope) = resources
                .invocation
                .code_index_schedulers
                .serving_code_scope(project)
                .await
            else {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };
            let Some(code) = scope.serving_generation else {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };
            if code.manifest().generation_id != *expected_source {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            let vector_id =
                match tracedecay_usecases::semantic_runtime::project_semantic_application_status(
                    project, None,
                )
                .map(|status| status.state)
                {
                    Some(SemanticRuntimeStateV1::Degraded {
                        active_generation: Some(generation),
                        ..
                    }) => generation,
                    Some(SemanticRuntimeStateV1::Current { receipt }) => {
                        receipt.activated_generation
                    }
                    _ => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                };
            let Some(provider) = resources
                .invocation
                .code_index_schedulers
                .semantic_vector_graph_provider(project)
                .await
            else {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };
            let Ok(retained) = provider.graph_for_generation(&code).await else {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };
            let Ok(Some(store)) =
                GraphVectorGenerationStoreV1::read_only_generation(&retained, &vector_id)
            else {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };
            let Ok(Some(vector)) = store
                .generation(&vector_id, Arc::clone(retained.cancellation()))
                .await
            else {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };
            if vector.source_generation() == expected_source {
                return (code, vector);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("production semantic generation did not publish")
}

pub(super) fn semantic_candidate(
    code: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    vector: &PublishedVectorGenerationV1,
) -> SemanticEvaluationProfileCandidateV1 {
    let material =
        crate::search_eval::load_default_evaluated_profile_material(EVALUATED_PROFILE_ID)
            .expect("checked-in evaluated profile material");
    let embedding = vector.embedding_key().embedding_key();
    let runtime_compatibility_digest = canonical_sha256(&(
        "tracedecay.semantic-runtime-compatibility.v1",
        &embedding.runtime_backend,
        &embedding.runtime_build_revision,
        embedding.device_class,
        embedding.precision,
    ))
    .expect("runtime compatibility digest");
    let search_index_key = SemanticSearchIndexProfileV1::exact_flat_v1()
        .and_then(|profile| profile.index_key())
        .expect("production exact-flat semantic index");
    // These bound the evaluation execution only. The accepted profile is
    // rebound to the evaluator's exact current/10x observations before it is
    // persisted or becomes activation-eligible.
    let evaluation_limits = crate::config::SemanticResourceCeilings::default();
    let vector_generation_id = vector.generation_id().clone();
    let calibration = SemanticCalibrationProfileV1 {
        calibration_profile_id: CalibrationProfileId::new(
            "calibration.semantic.native-product-journey.v1",
        )
        .expect("calibration profile id"),
        cohort_digest: canonical_sha256(&(
            "tracedecay.semantic.native-product-journey.cohort.v1",
            code.manifest().generation_id.clone(),
            vector_generation_id.clone(),
            code.capability().manifest_digest.clone(),
        ))
        .expect("calibration cohort digest"),
        projection_key: vector.projection_key().clone(),
        vector_generation: vector_generation_id.clone(),
        capability_manifest_digest: code.capability().manifest_digest.clone(),
        maximum_distance_micros: i64::MAX,
        minimum_margin_micros: 0,
    };
    SemanticEvaluationProfileCandidateV1 {
        evaluated_profile_id: EVALUATED_PROFILE_ID.to_owned(),
        profile: SemanticEvaluationFusionCandidateV1 {
            profile_id: material.profile.profile_id.clone(),
            calibrations: material.profile.calibrations.clone(),
            score_domain_calibrations: material.profile.score_domain_calibrations.clone(),
            weights_micros: material.profile.weights_micros.clone(),
            diversity_policy_id: material.profile.diversity_policy_id.clone(),
            rerank_policy_id: material.profile.rerank_policy_id.clone(),
            retrieval_budget: material.profile.retrieval_budget,
        },
        diversity: SemanticEvaluationDiversityCandidateV1 {
            policy_id: material.diversity.policy_id.clone(),
            per_source_namespace: material.diversity.per_source_namespace,
            per_source_instance: material.diversity.per_source_instance,
            per_repository: material.diversity.per_repository,
            per_file: material.diversity.per_file,
            per_session_or_thread: material.diversity.per_session_or_thread,
            per_copy_cluster: material.diversity.per_copy_cluster,
            per_evidence_role: material.diversity.per_evidence_role,
        },
        rerank: None,
        compatibility: RetrievalCompatibilityPinsV1 {
            semantic: Some(SemanticCompatibilityPinsV1 {
                implementation_revision: ComponentRevision::new("semantic.fastembed.production.v1")
                    .expect("semantic implementation revision"),
                fusion_revision: ComponentRevision::new(
                    "fusion.semantic.native-product-journey.v1",
                )
                .expect("fusion revision"),
                artifact_manifest_digest: embedding.model_artifact_digest.clone(),
                runtime_compatibility_digest,
                projection: vector.embedding_key().clone(),
                search_index_key,
                vector_generation_id,
                calibration,
                resources: SemanticResourceRequirementV1 {
                    model_bytes: evaluation_limits.max_model_bytes,
                    tokenizer_bytes: evaluation_limits.max_tokenizer_bytes,
                    resident_bytes: evaluation_limits.max_resident_bytes,
                    threads: evaluation_limits.max_threads,
                    max_concurrent_sessions: evaluation_limits.max_concurrent_sessions,
                    batch_size: evaluation_limits.max_batch_size,
                    sequence_length: evaluation_limits.max_sequence_length,
                    load_deadline_ms: evaluation_limits.load_deadline_ms,
                },
            }),
            rerank: None,
        },
    }
}

pub(super) async fn evaluate_native_profile(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    candidate: SemanticEvaluationProfileCandidateV1,
) -> ManifestDigest {
    let resources = harness.resources.as_ref().expect("live harness");
    let evaluation_limits = candidate
        .compatibility
        .semantic
        .as_ref()
        .expect("semantic evaluation limits")
        .resources;
    let observed_at = tracedecay_domain::UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_micros(),
        )
        .expect("evaluation time"),
    );
    let response = resources
        .invocation
        .service
        .invoke(
            &resources.invocation.lsp_session_registry,
            Some(project),
            None,
            None,
            None,
            crate::daemon_contract::DaemonInvocationRequest::semantic_evaluate_and_publish(
                format!(
                    "semantic-native-evaluation-{}",
                    candidate
                        .compatibility
                        .semantic
                        .as_ref()
                        .expect("semantic pins")
                        .vector_generation_id
                        .as_digest()
                        .as_str()
                ),
                candidate,
                observed_at,
                tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                    observed_at.0
                        + crate::daemon_client::SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS,
                ))
                .expect("evaluation deadline"),
                tracedecay_application::CancellationContext::active(
                    "cancellation.semantic-native-evaluation",
                )
                .expect("evaluation cancellation"),
            ),
        )
        .await;
    match response.outcome {
        crate::daemon_contract::DaemonInvocationOutcome::SemanticEvaluatedProfilePublished {
            profile_digest,
            report,
            ..
        } => {
            assert_eq!(
                report.status,
                crate::search_eval::DirectEvaluationStatusV1::Pass,
                "only a native evaluator PASS may enter activation"
            );
            let mut measured_projection_matrices = 0;
            for evidence in report
                .raw_outputs
                .iter()
                .filter_map(|output| output.native_resources.as_ref())
            {
                for result in evidence.samples.values() {
                    let crate::search_eval::semantic_native::SemanticNativeStageResultV1::Complete(
                        sample,
                    ) = result
                    else {
                        panic!("PASS resource sample must be complete");
                    };
                    assert_eq!(
                        sample.projection_cases.len(),
                        7,
                        "native evaluator must execute the exact seven-case matrix"
                    );
                    let cancellation = sample
                        .projection_cases
                        .get(
                            &crate::search_eval::semantic_native::SemanticProjectionCaseV1::Cancellation,
                        )
                        .expect("cancellation case");
                    assert!(
                        cancellation.chunks_added_or_changed > 0
                            && cancellation.projection_calls > 0
                            && cancellation.projection_calls < cancellation.chunks_added_or_changed,
                        "cancellation must stop after observed partial projection work"
                    );
                    measured_projection_matrices += 1;
                }
            }
            assert!(
                measured_projection_matrices > 0,
                "PASS must retain at least one real seven-case projection matrix"
            );
            let measured = report
                .semantic_activation_resource_pins(EVALUATED_PROFILE_ID)
                .expect("PASS carries exact current/10x resource pins");
            let lifecycle =
                crate::semantic_code::shared_lifecycle_owner().expect("production lifecycle");
            let model = lifecycle
                .catalog()
                .get(crate::semantic_code::DEFAULT_FASTEMBED_MODEL_ID)
                .expect("default model manifest");
            assert_eq!(
                measured.model_bytes, model.members["model"].length,
                "accepted model bytes come from the evaluated artifact"
            );
            assert_eq!(
                measured.tokenizer_bytes, model.members["tokenizer"].length,
                "accepted tokenizer bytes come from the evaluated artifact"
            );
            assert!(measured.model_bytes < evaluation_limits.model_bytes);
            assert!(measured.tokenizer_bytes < evaluation_limits.tokenizer_bytes);
            assert!(measured.resident_bytes >= measured.model_bytes);
            assert!(measured.resident_bytes >= measured.tokenizer_bytes);
            assert!(measured.resident_bytes <= evaluation_limits.resident_bytes);
            assert_eq!(measured.threads, evaluation_limits.threads);
            assert_eq!(
                measured.max_concurrent_sessions, 1,
                "native evaluation measures one real model session"
            );
            assert!(
                measured.max_concurrent_sessions <= evaluation_limits.max_concurrent_sessions,
                "measured model sessions must fit the configured ceiling"
            );
            assert_eq!(measured.batch_size, evaluation_limits.batch_size);
            assert_eq!(measured.sequence_length, evaluation_limits.sequence_length);
            assert_eq!(
                measured.load_deadline_ms,
                evaluation_limits.load_deadline_ms
            );
            profile_digest
        }
        outcome => panic!("native semantic profile publication failed: {outcome:?}"),
    }
}

async fn current_configuration_revision(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> tracedecay_domain::configuration::ConfigurationRevisionId {
    harness
        .server(project)
        .expect("project server")
        .cg()
        .await
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("current production configuration")
        .revision_id
}

pub(super) async fn set_semantic_profile(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    active: crate::config::SemanticProfileSelection,
    rollback: Option<crate::config::SemanticProfileSelection>,
) {
    let expected_revision = current_configuration_revision(harness, project).await;
    let request = ConfigurationSetRequestV1 {
        layer: ConfigurationLayerIdV1::Default,
        key: SettingKey::new(crate::config::SEMANTIC_RUNTIME_SETTING_KEY)
            .expect("semantic runtime setting key"),
        value: ConfigurationValueV1::Text(
            serde_json::to_string(&crate::config::SemanticConfig {
                selected_model: Some(crate::semantic_code::DEFAULT_FASTEMBED_MODEL_ID.to_owned()),
                auto_download: false,
                active_profile: Some(active),
                rollback_profile: rollback,
                resources: crate::config::SemanticResourceCeilings::default(),
            })
            .expect("semantic runtime JSON"),
        ),
        idempotency_key: tracedecay_domain::configuration::ConfigurationIdempotencyKey::new(
            format!("configuration.idempotency.semantic-activation.{expected_revision}"),
        )
        .expect("semantic configuration idempotency key"),
        expected_revision,
    };
    let response = harness
        .call_tool(
            project,
            "tracedecay_configuration_set",
            serde_json::to_value(request).expect("configuration set request"),
        )
        .await
        .expect("public semantic configuration mutation");
    assert_tool_effect_succeeded(&response);
}

async fn search(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    strict: bool,
) -> Value {
    let mut arguments = json!({
        "query": "semantic_product_probe",
        "limit": 10,
        "format": "json"
    });
    if strict {
        arguments["semantic_mode"] = json!("strict_semantic");
    }
    tool_payload(
        &harness
            .call_tool(project, "tracedecay_search", arguments)
            .await
            .expect("public production search"),
    )
}

async fn semantic_runtime_status(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Value {
    tool_payload(
        &harness
            .call_tool(project, "tracedecay_runtime", json!({}))
            .await
            .expect("public production runtime status"),
    )["semantic_runtime"]
        .clone()
}

async fn graph_bytes(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    generations: &[(
        Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>,
        VectorGenerationIdV1,
    )],
) -> Vec<u8> {
    let resources = harness.resources.as_ref().expect("live harness");
    let provider = resources
        .invocation
        .code_index_schedulers
        .semantic_vector_graph_provider(project)
        .await
        .expect("daemon semantic vector graph provider");
    let mut snapshots = Vec::new();
    for (code, vector_id) in generations {
        let retained = provider
            .graph_for_generation(code)
            .await
            .expect("retain exact semantic vector graph");
        let store = GraphVectorGenerationStoreV1::read_only_generation(&retained, vector_id)
            .expect("read exact vector generation")
            .expect("published vector generation");
        let generation = store
            .generation(vector_id, Arc::clone(retained.cancellation()))
            .await
            .expect("read vector generation catalog")
            .expect("cataloged vector generation");
        let head = retained
            .runtime()
            .verified_head(
                &tracedecay_usecases::semantic_runtime::SemanticGraphExecutionAuthorityV1::new(
                    Arc::clone(retained.cancellation()),
                    std::time::Instant::now() + Duration::from_secs(10),
                ),
            )
            .expect("verified semantic graph head")
            .expect("published semantic graph head");
        snapshots.push((
            code.manifest().generation_id.clone(),
            vector_id.clone(),
            store
                .verified_revision(Arc::clone(retained.cancellation()))
                .expect("verified semantic graph revision"),
            head,
            generation,
        ));
    }
    serde_json::to_vec(&snapshots).expect("canonical graph authority snapshot")
}

pub(super) fn selection(
    digest: ManifestDigest,
    artifact_digest: &str,
    artifact_path: &Path,
) -> crate::config::SemanticProfileSelection {
    crate::config::SemanticProfileSelection {
        profile_id: EVALUATED_PROFILE_ID.to_owned(),
        accepted_profile_digest: digest,
        artifact_digest: artifact_digest.to_owned(),
        artifact_path: artifact_path.to_path_buf(),
    }
}

async fn assert_code_generation_unchanged(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    expected: &tracedecay_domain::CodeGenerationId,
) {
    assert_eq!(
        harness
            .resources
            .as_ref()
            .expect("live harness")
            .invocation
            .code_index_schedulers
            .latest_generation_id(project)
            .await,
        Some(expected.clone()),
        "semantic configuration must not publish a code-index generation"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_semantic_activation_rollback_and_exact_retry_preserve_graph_authority() {
    let fixture_root = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .expect(
            "semantic activation product journey requires \
             TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE from distribution acceptance",
        );
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

    let isolation = tempfile::TempDir::new().expect("journey isolation");
    let project = isolation.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("source directory");
    git(&project, &["init", "--quiet"]);
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn semantic_product_probe() -> &'static str { \"generation-one\" }\n",
    )
    .expect("G1 source");
    let first_commit = commit(&project, "test: seed semantic generation one");

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");
    let resources = harness.resources.as_ref().expect("live harness");
    let first_code_id = resources
        .invocation
        .code_index_schedulers
        .latest_generation_id(&project)
        .await
        .expect("G1 code generation");
    let (first_code, first_vector) =
        wait_for_semantic_generation(&harness, &project, &first_code_id).await;
    let first_generation = [(
        Arc::clone(&first_code),
        first_vector.generation_id().clone(),
    )];
    let graph_before_first_evaluation = graph_bytes(&harness, &project, &first_generation).await;
    let first_profile = evaluate_native_profile(
        &harness,
        &project,
        semantic_candidate(&first_code, &first_vector),
    )
    .await;
    assert_eq!(
        graph_bytes(&harness, &project, &first_generation).await,
        graph_before_first_evaluation,
        "native evaluation must not publish into the project graph"
    );
    set_semantic_profile(
        &harness,
        &project,
        selection(first_profile.clone(), &artifact_digest, &artifact_path),
        None,
    )
    .await;
    assert_code_generation_unchanged(&harness, &project, &first_code_id).await;
    let first_query = search(&harness, &project, true).await;
    assert_eq!(first_query["semantic"]["status"], "complete");
    assert_eq!(
        first_query["code_generation"],
        json!(first_code.manifest().generation_id)
    );

    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn semantic_product_probe() -> &'static str { \"generation-two\" }\n",
    )
    .expect("G2 source");
    let second_commit = commit(&project, "test: publish semantic generation two");
    assert!(
        resources
            .invocation
            .code_index_schedulers
            .notify_hook_paths(&project, &["src/lib.rs".to_owned()])
            .await
    );
    let second_code_id = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let generation = resources
                .invocation
                .code_index_schedulers
                .latest_generation_id(&project)
                .await;
            if generation
                .as_ref()
                .is_some_and(|generation| generation != &first_code_id)
            {
                return generation.expect("G2 generation");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("G2 code generation did not publish");
    let (second_code, second_vector) =
        wait_for_semantic_generation(&harness, &project, &second_code_id).await;
    assert_ne!(first_vector.generation_id(), second_vector.generation_id());
    let generations = [
        (
            Arc::clone(&first_code),
            first_vector.generation_id().clone(),
        ),
        (
            Arc::clone(&second_code),
            second_vector.generation_id().clone(),
        ),
    ];
    let graph_before_second_evaluation = graph_bytes(&harness, &project, &generations).await;
    let second_profile = evaluate_native_profile(
        &harness,
        &project,
        semantic_candidate(&second_code, &second_vector),
    )
    .await;
    assert_eq!(
        graph_bytes(&harness, &project, &generations).await,
        graph_before_second_evaluation,
        "native reevaluation must not publish into the project graph"
    );
    let graph_before_activation = graph_bytes(&harness, &project, &generations).await;
    set_semantic_profile(
        &harness,
        &project,
        selection(second_profile.clone(), &artifact_digest, &artifact_path),
        Some(selection(
            first_profile.clone(),
            &artifact_digest,
            &artifact_path,
        )),
    )
    .await;
    assert_code_generation_unchanged(&harness, &project, &second_code_id).await;
    assert_eq!(
        graph_bytes(&harness, &project, &generations).await,
        graph_before_activation,
        "activation must not publish or rewrite graph state"
    );
    let second_query = search(&harness, &project, true).await;
    assert_eq!(second_query["semantic"]["status"], "complete");
    assert_eq!(
        second_query["code_generation"],
        json!(second_code.manifest().generation_id)
    );

    git(
        &project,
        &["checkout", "--quiet", "--detach", &first_commit],
    );
    assert!(
        resources
            .invocation
            .code_index_schedulers
            .notify_hook_paths(&project, &["src/lib.rs".to_owned()])
            .await
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        while resources
            .invocation
            .code_index_schedulers
            .latest_generation_id(&project)
            .await
            .as_ref()
            != Some(&first_code_id)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("G1 code generation did not restore");
    set_semantic_profile(
        &harness,
        &project,
        selection(first_profile.clone(), &artifact_digest, &artifact_path),
        Some(selection(
            second_profile.clone(),
            &artifact_digest,
            &artifact_path,
        )),
    )
    .await;
    assert_code_generation_unchanged(&harness, &project, &first_code_id).await;
    assert_eq!(
        graph_bytes(&harness, &project, &generations).await,
        graph_before_activation,
        "rollback must preserve the graph catalog, control state, and verified heads byte-for-byte"
    );
    let rolled_back_query = search(&harness, &project, true).await;
    assert_eq!(rolled_back_query["semantic"]["status"], "complete");
    assert_eq!(
        rolled_back_query["code_generation"],
        json!(first_code.manifest().generation_id)
    );

    git(
        &project,
        &["checkout", "--quiet", "--detach", &second_commit],
    );
    assert!(
        resources
            .invocation
            .code_index_schedulers
            .notify_hook_paths(&project, &["src/lib.rs".to_owned()])
            .await
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        while resources
            .invocation
            .code_index_schedulers
            .latest_generation_id(&project)
            .await
            .as_ref()
            != Some(&second_code_id)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("G2 code generation did not restore");
    let core_before_failure = search(&harness, &project, false).await;
    assert_ne!(core_before_failure["semantic"]["status"], "complete");
    assert!(
        tracedecay_usecases::semantic_runtime::unbind_project_semantic_cache_if_current(
            &project,
            second_vector.generation_id(),
        )
    );
    lifecycle
        .mark_runtime_failed("injected live install failure", true)
        .expect("inject live install failure");
    set_semantic_profile(
        &harness,
        &project,
        selection(second_profile.clone(), &artifact_digest, &artifact_path),
        Some(selection(first_profile, &artifact_digest, &artifact_path)),
    )
    .await;
    assert_code_generation_unchanged(&harness, &project, &second_code_id).await;
    let core_during_failure = search(&harness, &project, false).await;
    assert_eq!(
        core_during_failure["query_fallback_digest"], core_before_failure["query_fallback_digest"],
        "failed semantic observation must preserve the canonical core query bytes"
    );
    assert_eq!(
        core_during_failure["results"], core_before_failure["results"],
        "failed semantic observation must preserve ordinary exact/lexical/graph results"
    );
    assert_ne!(core_during_failure["semantic"]["status"], "complete");
    let strict_during_failure = search(&harness, &project, true).await;
    assert_eq!(strict_during_failure["status"], "unavailable");
    assert_ne!(strict_during_failure["semantic"]["status"], "complete");
    assert_ne!(
        semantic_runtime_status(&harness, &project).await["state"],
        "ready",
        "failed observation must remain visibly degraded"
    );
    assert_eq!(
        graph_bytes(&harness, &project, &generations).await,
        graph_before_activation,
        "failed live install must not mutate graph publication authority"
    );

    lifecycle
        .retry()
        .expect("re-admit verified installed model");
    let (recovered, recovered_status) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = search(&harness, &project, true).await;
            let status = semantic_runtime_status(&harness, &project).await;
            if result["semantic"]["status"] == "complete" && status["state"] == "ready" {
                return (result, status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("daemon semantic activation recovery did not converge");
    assert_eq!(recovered["semantic"]["status"], "complete");
    assert_eq!(recovered_status["state"], "ready");
    assert_eq!(
        recovered_status["receipt"]["activated_generation"],
        json!(second_vector.generation_id())
    );
    assert_eq!(
        recovered["code_generation"],
        json!(second_code.manifest().generation_id)
    );
    assert_eq!(
        graph_bytes(&harness, &project, &generations).await,
        graph_before_activation,
        "exact retry must restore routing without graph publication"
    );
    assert_code_generation_unchanged(&harness, &project, &second_code_id).await;
    harness.shutdown().await;
}

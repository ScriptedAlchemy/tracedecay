#![cfg(feature = "semantic-fastembed")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_application::ConfigurationSetRequestV1;
use tracedecay_domain::configuration::{ConfigurationLayerIdV1, ConfigurationValueV1, SettingKey};
use tracedecay_domain::{ManifestDigest, VectorGenerationIdV1};
use tracedecay_semantic_contracts::{
    DEFAULT_FASTEMBED_MODEL_ID, SemanticConfig, SemanticModelLifecycleStateV1,
    SemanticProfileSelection, SemanticResourceCeilings,
};
use tracedecay_usecases::semantic_runtime::{ProjectSemanticActivationExt, SemanticRuntimeStateV1};
use tracedecay_usecases::store::vector_generations::{
    GraphVectorGenerationStoreV1, PublishedVectorGenerationV1,
};

use super::journey_test_support::{git, tool_payload};
use super::*;

const EVALUATED_PROFILE_ID: &str = "hybrid-conservative";

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

fn assert_tool_effect_succeeded(response: &JsonRpcResponse) {
    assert!(response.error.is_none(), "tool failed: {response:?}");
    let result = response.result.as_ref().expect("tool result");
    assert_ne!(result["isError"], true, "tool effect failed: {result}");
}

pub(super) fn seed_distribution_fixture(
    lifecycle_root: &Path,
    fixture_root: &Path,
    owner: &tracedecay_semantic::SemanticModelLifecycleOwnerV1,
) {
    let model = owner
        .catalog()
        .get(DEFAULT_FASTEMBED_MODEL_ID)
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
    owner: &tracedecay_semantic::SemanticModelLifecycleOwnerV1,
) -> (String, PathBuf) {
    match owner.status().state.expect("installed model state") {
        SemanticModelLifecycleStateV1::Installed {
            artifact_digest,
            install_path,
            ..
        }
        | SemanticModelLifecycleStateV1::Ready {
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
            // A serving generation is deliberately available before the
            // scheduler seals it. Native profile evaluation is stricter: its
            // snapshot authority accepts only the exact complete-and-fresh
            // generation. Wait for the public status of that authority rather
            // than racing an evaluation against publication settlement.
            let freshness = tool_payload(
                &harness
                    .call_tool(
                        project,
                        "tracedecay_status",
                        json!({
                            "format": "json",
                            "include_branch_diagnostics": false,
                            "include_storage_health": false,
                            "include_session_ingest": false,
                            "include_staleness": false,
                        }),
                    )
                    .await
                    .expect("public code-index readiness status"),
            );
            if freshness["code_index_freshness"]["status"] != json!("current")
                || freshness["code_index_freshness"]["worktree"]["latest_generation_id"]
                    != json!(expected_source)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            // Complete status alone is not enough: a sealed generation must
            // be paired with a live code-index query authority before an
            // evaluator may consume it. This ordinary public query is the
            // authority preflight; an unavailable outcome keeps waiting and
            // is never treated as a successful evaluation precondition.
            let query_readiness = tool_payload(
                &harness
                    .call_tool(
                        project,
                        "tracedecay_search",
                        json!({"query": "fn", "limit": 1, "format": "json"}),
                    )
                    .await
                    .expect("public code-index query-authority readiness"),
            );
            if query_readiness["status"] == json!("unavailable")
                || query_readiness["code_generation"] != json!(expected_source)
            {
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
                let lifecycle =
                    tracedecay_usecases::semantic_runtime::project_or_shared_lifecycle_status(
                        project,
                    )
                    .expect("production lifecycle status");
                if matches!(
                    lifecycle.state,
                    Some(SemanticModelLifecycleStateV1::Ready { .. })
                ) {
                    return (code, vector);
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("production semantic generation did not publish")
}

async fn wait_for_settled_semantic_generation(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    prior: Option<&tracedecay_domain::CodeGenerationId>,
) -> (
    tracedecay_domain::CodeGenerationId,
    Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>,
    PublishedVectorGenerationV1,
) {
    tokio::time::timeout(Duration::from_mins(4), async {
        loop {
            let generation_id = harness
                .resources
                .as_ref()
                .expect("live harness")
                .invocation
                .code_index_schedulers
                .latest_generation_id(project)
                .await
                .expect("code generation");
            if prior == Some(&generation_id) {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            let (_code, vector) =
                wait_for_semantic_generation(harness, project, &generation_id).await;
            tokio::time::sleep(Duration::from_secs(10)).await;
            if harness
                .resources
                .as_ref()
                .expect("live harness")
                .invocation
                .code_index_schedulers
                .latest_generation_id(project)
                .await
                .as_ref()
                == Some(&generation_id)
            {
                let (settled_code, settled_vector) =
                    wait_for_semantic_generation(harness, project, &generation_id).await;
                if settled_vector.generation_id() == vector.generation_id() {
                    return (generation_id, settled_code, settled_vector);
                }
            }
        }
    })
    .await
    .expect("production semantic generation did not settle")
}

pub(super) async fn evaluate_native_profile(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> ManifestDigest {
    let resources = harness.resources.as_ref().expect("live harness");
    for attempt in 0..2 {
        let evaluation_limits = SemanticResourceCeilings::default();
        let observed_at = tracedecay_domain::UtcMicros(
            i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system time")
                    .as_micros(),
            )
            .expect("evaluation time"),
        );
        // Exercise the same identity authority the production client uses. A
        // vector generation is a `sha256:<hex>` digest, which cannot be embedded
        // in a daemon request token; doing so truthfully fails at request
        // validation before the evaluator is reached.
        let request_id = tracedecay_application::request_identity::mint_global_request_id(
            tracedecay_application::request_identity::GlobalRequestSurface::SemanticEvaluation,
        )
        .expect("mint a production semantic-evaluation request id");
        let response = resources
        .invocation
        .service
        .invoke(
            &resources.invocation.lsp_session_registry,
            Some(project),
            None,
            None,
            None,
            tracedecay_daemon_protocol::DaemonInvocationRequest::semantic_evaluate_and_publish(
                request_id.as_str(),
                EVALUATED_PROFILE_ID.to_owned(),
                observed_at,
                tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                    observed_at.0
                        + tracedecay_daemon_protocol::SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS,
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
        tracedecay_daemon_protocol::DaemonInvocationOutcome::SemanticEvaluatedProfilePublished {
            profile_digest,
            report,
            ..
        } => {
            let report: tracedecay_query::search_quality::DirectEvaluationReportV1 =
                serde_json::from_value(report).expect("direct evaluation report wire");
            assert_eq!(
                report.status,
                tracedecay_query::search_quality::DirectEvaluationStatusV1::Pass,
                "only a native evaluator PASS may enter activation"
            );
            let mut measured_projection_matrices = 0;
            for evidence in report
                .raw_outputs
                .iter()
                .filter_map(|output| output.native_resources.as_ref())
            {
                for result in evidence.samples.values() {
                    let tracedecay_query::search_quality::semantic_native::SemanticNativeStageResultV1::Complete(
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
                            &tracedecay_query::search_quality::semantic_native::SemanticProjectionCaseV1::Cancellation,
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
                tracedecay_semantic::default_shared_lifecycle_owner().expect("production lifecycle");
            let model = lifecycle
                .catalog()
                .get(DEFAULT_FASTEMBED_MODEL_ID)
                .expect("default model manifest");
            assert_eq!(
                measured.model_bytes, model.members["model"].length,
                "accepted model bytes come from the evaluated artifact"
            );
            assert_eq!(
                measured.tokenizer_bytes, model.members["tokenizer"].length,
                "accepted tokenizer bytes come from the evaluated artifact"
            );
            assert!(measured.model_bytes < evaluation_limits.max_model_bytes);
            assert!(measured.tokenizer_bytes < evaluation_limits.max_tokenizer_bytes);
            assert!(measured.resident_bytes >= measured.model_bytes);
            assert!(measured.resident_bytes >= measured.tokenizer_bytes);
            assert!(measured.resident_bytes <= evaluation_limits.max_resident_bytes);
            assert_eq!(measured.threads, evaluation_limits.max_threads);
            assert_ne!(
                measured.max_concurrent_sessions, 0,
                "native evaluation must measure at least one real model session"
            );
            assert!(
                measured.max_concurrent_sessions <= evaluation_limits.max_concurrent_sessions,
                "measured model sessions must fit the configured ceiling"
            );
            assert_eq!(measured.batch_size, evaluation_limits.max_batch_size);
            assert_eq!(
                measured.sequence_length,
                evaluation_limits.max_sequence_length
            );
            assert_eq!(
                measured.load_deadline_ms,
                evaluation_limits.load_deadline_ms
            );
                return profile_digest;
            }
            tracedecay_daemon_protocol::DaemonInvocationOutcome::ApplicationProblem {
                problem: tracedecay_application::ApplicationProblem::Conflict { .. },
            } if attempt == 0 => continue,
            outcome => panic!("native semantic profile publication failed: {outcome:?}"),
        }
    }
    unreachable!("bounded semantic evaluation retry returned no outcome")
}

async fn activate_native_profile(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> ManifestDigest {
    let resources = harness.resources.as_ref().expect("live harness");
    for attempt in 0..2 {
        let observed_at = tracedecay_domain::UtcMicros(
            i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system time")
                    .as_micros(),
            )
            .expect("activation time"),
        );
        let request_id = tracedecay_application::request_identity::mint_global_request_id(
            tracedecay_application::request_identity::GlobalRequestSurface::SemanticEvaluation,
        )
        .expect("mint a production semantic-activation request id");
        let response = resources
            .invocation
            .service
            .invoke(
                &resources.invocation.lsp_session_registry,
                Some(project),
                None,
                None,
                None,
                tracedecay_daemon_protocol::DaemonInvocationRequest::semantic_activate(
                    request_id.as_str(),
                    EVALUATED_PROFILE_ID.to_owned(),
                    true,
                    observed_at,
                    tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                        observed_at.0
                            + tracedecay_daemon_protocol::SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS,
                    ))
                    .expect("activation deadline"),
                    tracedecay_application::CancellationContext::active(
                        "cancellation.semantic-native-activation",
                    )
                    .expect("activation cancellation"),
                ),
            )
            .await;
        match response.outcome {
            tracedecay_daemon_protocol::DaemonInvocationOutcome::SemanticProfileActivated {
                profile_digest,
                report_digest,
                rollback_profile_id,
                runtime_state,
                ..
            } => {
                assert!(
                    !report_digest.as_str().is_empty(),
                    "activation receipt must identify its calibration report"
                );
                assert_eq!(
                    rollback_profile_id, None,
                    "fresh activation has no prior profile to retain"
                );
                assert!(
                    runtime_state.get("state").is_some(),
                    "activation receipt must expose the typed runtime state: {runtime_state}"
                );
                return profile_digest;
            }
            tracedecay_daemon_protocol::DaemonInvocationOutcome::ApplicationProblem {
                problem: tracedecay_application::ApplicationProblem::Conflict { .. },
            } if attempt == 0 => continue,
            outcome => panic!("composed semantic activation failed: {outcome:?}"),
        }
    }
    unreachable!("bounded semantic activation retry returned no outcome")
}

pub(super) async fn set_semantic_profile(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    active: SemanticProfileSelection,
    rollback: Option<SemanticProfileSelection>,
) {
    let graph = harness.server(project).expect("project server").cg().await;
    let project_id = graph
        .configuration_runtime()
        .configuration_target()
        .project_id
        .clone();
    let expected_revision = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("current production configuration")
        .revision_id;
    let request = ConfigurationSetRequestV1 {
        layer: ConfigurationLayerIdV1::Project { project_id },
        key: SettingKey::new(crate::config::SEMANTIC_RUNTIME_SETTING_KEY)
            .expect("semantic runtime setting key"),
        value: ConfigurationValueV1::Text(
            serde_json::to_string(&SemanticConfig {
                selected_model: Some(DEFAULT_FASTEMBED_MODEL_ID.to_owned()),
                auto_download: false,
                active_profile: Some(active),
                rollback_profile: rollback,
                resources: SemanticResourceCeilings::default(),
                document_composition:
                    tracedecay_domain::EmbeddingDocumentCompositionV1::SanitizedText,
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

async fn strict_unavailable_search(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Value {
    let response = harness
        .call_tool(
            project,
            "tracedecay_search",
            json!({
                "query": "semantic_product_probe",
                "limit": 10,
                "format": "json",
                "semantic_mode": "strict_semantic",
            }),
        )
        .await
        .expect("strict semantic failure answers with a typed payload");
    assert!(
        response.error.is_none(),
        "strict semantic unavailability must not become a transport error: {response:?}"
    );
    let result = response.result.as_ref().expect("strict semantic result");
    assert_eq!(
        result["isError"],
        json!(true),
        "strict semantic unavailability must remain a typed tool failure: {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .expect("strict semantic tool text");
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("strict semantic tool did not return JSON: {error}; result={result}; text={text}")
    })
}

/// Proves that semantic execution contributed to the actual source probe,
/// rather than merely reporting a ready runtime or complete lane.
pub(super) fn assert_semantic_probe_contribution(
    payload: &Value,
    probe_symbol: &str,
    checkpoint: &str,
) {
    let results = payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("{checkpoint} returned no result list: {payload}"));
    let probe = results
        .iter()
        .find(|result| result["display"]["name"] == json!(probe_symbol))
        .unwrap_or_else(|| {
            panic!("{checkpoint} did not return source probe {probe_symbol}: {payload}")
        });
    let contributions = probe["candidate"]["contributions"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "{checkpoint} returned probe {probe_symbol} without candidate contributions: {probe}"
            )
        });
    assert!(
        contributions
            .iter()
            .any(|contribution| contribution["retriever"] == json!("semantic")),
        "{checkpoint} returned probe {probe_symbol} without a semantic contribution: {probe}"
    );
}

async fn semantic_runtime_status(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Value {
    tool_payload(
        &harness
            .call_tool(project, "tracedecay_runtime", json!({"format": "json"}))
            .await
            .expect("public production runtime status"),
    )["semantic_runtime"]
        .clone()
}

async fn wait_for_semantic_runtime_ready(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Value {
    let mut latest = semantic_runtime_status(harness, project).await;
    let ready = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let status = semantic_runtime_status(harness, project).await;
            if status["state"]["state"] == "ready" {
                return status;
            }
            latest = status;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    ready.unwrap_or_else(|_| {
        panic!("semantic activation did not converge to ready: latest status {latest}")
    })
}

async fn retain_graph(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
) -> tracedecay_usecases::semantic_runtime::RetainedSemanticVectorGraphV1 {
    harness
        .resources
        .as_ref()
        .expect("live harness")
        .invocation
        .code_index_schedulers
        .semantic_vector_graph_provider(project)
        .await
        .expect("daemon semantic vector graph provider")
        .graph_for_generation(generation)
        .await
        .expect("retain exact semantic vector graph")
}

async fn graph_bytes(
    generations: &[(
        &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
        &tracedecay_usecases::semantic_runtime::RetainedSemanticVectorGraphV1,
        VectorGenerationIdV1,
    )],
) -> Vec<u8> {
    let mut snapshots = Vec::new();
    for (code, retained, vector_id) in generations {
        let store = GraphVectorGenerationStoreV1::read_only_generation(retained, vector_id)
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
        // Graph reconstruction hydrates ExternalV1 collections by value and
        // never assigns persist-document content addresses. Serializing
        // `PublishedVectorGenerationV1` through its state-document adapters
        // therefore fails closed ("serialized before it was sealed"). Snapshot
        // catalog identity, verified head, and collection values instead.
        snapshots.push((
            code.manifest().generation_id.clone(),
            vector_id.clone(),
            store
                .verified_revision(Arc::clone(retained.cancellation()))
                .expect("verified semantic graph revision"),
            head,
            generation.generation_id().clone(),
            generation.projection_key().clone(),
            generation.source_generation().clone(),
            generation.source_manifest_digest().clone(),
            generation.base_generation().cloned(),
            generation.embedding_key().clone(),
            generation.checkpoint().clone(),
            generation.manifest_digest().clone(),
            generation.vectors().clone(),
            generation.tombstones().to_vec(),
            generation.tombstone_digests().clone(),
            generation.receipts().to_vec(),
        ));
    }
    serde_json::to_vec(&snapshots).expect("canonical graph authority snapshot")
}

pub(super) fn selection(
    digest: ManifestDigest,
    artifact_digest: &str,
    artifact_path: &Path,
) -> SemanticProfileSelection {
    SemanticProfileSelection {
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
    // The journey needs the byte-pinned FastEmbed package from distribution
    // acceptance; it cannot be synthesized, and the ordinary test lane has no
    // reason to have it. Skip explicitly rather than fail the lane, matching
    // `semantic_availability_journey_test`.
    let Some(fixture_root) = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    else {
        eprintln!(
            "skipping the semantic activation product journey; prepare the \
             distribution-acceptance package and set \
             TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE"
        );
        return;
    };
    let _profile = crate::config::PinnedUserDataDir::new();
    let lifecycle_root =
        tracedecay_semantic::default_lifecycle_root().expect("isolated lifecycle root");
    let lifecycle =
        tracedecay_semantic::default_shared_lifecycle_owner().expect("production lifecycle owner");
    seed_distribution_fixture(&lifecycle_root, &fixture_root, &lifecycle);
    lifecycle
        .select_model(Some(DEFAULT_FASTEMBED_MODEL_ID), true)
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
    let (first_code_id, first_code, first_vector) =
        wait_for_settled_semantic_generation(&harness, &project, None).await;
    let graph = harness.server(&project).expect("project server").cg().await;
    assert!(
        graph
            .configuration_runtime()
            .semantic_activation_coordinator()
            .is_some(),
        "semantic activation coordinator must be mounted before evaluation"
    );
    let first_graph = retain_graph(&harness, &project, &first_code).await;
    let first_generation = [(
        first_code.as_ref(),
        &first_graph,
        first_vector.generation_id().clone(),
    )];
    let graph_before_first_evaluation = graph_bytes(&first_generation).await;
    let first_profile = activate_native_profile(&harness, &project).await;
    let first_runtime = wait_for_semantic_runtime_ready(&harness, &project).await;
    assert_eq!(first_runtime["state"]["state"], "ready");
    assert_code_generation_unchanged(&harness, &project, &first_code_id).await;
    assert_eq!(
        graph_bytes(&first_generation).await,
        graph_before_first_evaluation,
        "composed evaluation and activation must not publish into the project graph"
    );
    let first_query = search(&harness, &project, true).await;
    assert_eq!(first_query["semantic"]["status"], "complete");
    assert_semantic_probe_contribution(
        &first_query,
        "semantic_product_probe",
        "first semantic activation",
    );
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
    let (second_code_id, second_code, second_vector) =
        wait_for_settled_semantic_generation(&harness, &project, Some(&first_code_id)).await;
    assert_ne!(first_vector.generation_id(), second_vector.generation_id());
    let second_graph = retain_graph(&harness, &project, &second_code).await;
    let generations = [
        (
            first_code.as_ref(),
            &first_graph,
            first_vector.generation_id().clone(),
        ),
        (
            second_code.as_ref(),
            &second_graph,
            second_vector.generation_id().clone(),
        ),
    ];
    let graph_before_second_evaluation = graph_bytes(&generations).await;
    let second_profile = evaluate_native_profile(&harness, &project).await;
    assert_eq!(
        graph_bytes(&generations).await,
        graph_before_second_evaluation,
        "native reevaluation must not publish into the project graph"
    );
    let graph_before_activation = graph_bytes(&generations).await;
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
    let second_runtime = wait_for_semantic_runtime_ready(&harness, &project).await;
    assert_eq!(
        second_runtime["state"]["receipt"]["activated_generation"],
        json!(second_vector.generation_id())
    );
    assert_code_generation_unchanged(&harness, &project, &second_code_id).await;
    assert_eq!(
        graph_bytes(&generations).await,
        graph_before_activation,
        "activation must not publish or rewrite graph state"
    );
    let second_query = search(&harness, &project, true).await;
    assert_eq!(second_query["semantic"]["status"], "complete");
    assert_semantic_probe_contribution(
        &second_query,
        "semantic_product_probe",
        "second semantic activation",
    );
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
    let rollback_runtime = wait_for_semantic_runtime_ready(&harness, &project).await;
    assert_eq!(
        rollback_runtime["state"]["receipt"]["activated_generation"],
        json!(first_vector.generation_id())
    );
    assert_code_generation_unchanged(&harness, &project, &first_code_id).await;
    assert_eq!(
        graph_bytes(&generations).await,
        graph_before_activation,
        "rollback must preserve the graph catalog, control state, and verified heads byte-for-byte"
    );
    let rolled_back_query = search(&harness, &project, true).await;
    assert_eq!(rolled_back_query["semantic"]["status"], "complete");
    assert_semantic_probe_contribution(
        &rolled_back_query,
        "semantic_product_probe",
        "semantic rollback",
    );
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
    let strict_during_failure = strict_unavailable_search(&harness, &project).await;
    assert_eq!(strict_during_failure["status"], "unavailable");
    assert_ne!(strict_during_failure["semantic"]["status"], "complete");
    assert_ne!(
        semantic_runtime_status(&harness, &project).await["state"]["state"],
        "ready",
        "failed observation must remain visibly degraded"
    );
    assert_eq!(
        graph_bytes(&generations).await,
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
            if result["semantic"]["status"] == "complete" && status["state"]["state"] == "ready" {
                return (result, status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("daemon semantic activation recovery did not converge");
    assert_eq!(recovered["semantic"]["status"], "complete");
    assert_semantic_probe_contribution(&recovered, "semantic_product_probe", "semantic retry");
    assert_eq!(recovered_status["state"]["state"], "ready");
    assert_eq!(
        recovered_status["state"]["receipt"]["activated_generation"],
        json!(second_vector.generation_id())
    );
    assert_eq!(
        recovered["code_generation"],
        json!(second_code.manifest().generation_id)
    );
    assert_eq!(
        graph_bytes(&generations).await,
        graph_before_activation,
        "exact retry must restore routing without graph publication"
    );
    assert_code_generation_unchanged(&harness, &project, &second_code_id).await;
    harness.shutdown().await;
}

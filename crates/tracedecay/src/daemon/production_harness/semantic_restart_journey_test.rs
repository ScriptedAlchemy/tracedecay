//! Live journey (#753, success test 3): fresh install → evaluated activation
//! → strict semantic query → daemon restart → the same strict query again.
//!
//! The contract under test is lifecycle authority after a restart. The
//! activation receipt, the retained vector generation, and the serving code
//! generation are re-seated from durable state by a new composition; the
//! semantic lane must answer again from the *same* evaluated vector generation
//! (no store wipe, no forced rebuild) within a bounded window (no indefinite
//! loading), and it must do so whether or not the restarted code index mints a
//! new generation identifier for the unchanged source bytes.

#![cfg(feature = "semantic-fastembed")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_semantic_contracts::DEFAULT_FASTEMBED_MODEL_ID;
use tracedecay_usecases::semantic_runtime::{
    SemanticSourceCoherenceOutcomeV1, semantic_source_coherence,
};
use tracedecay_usecases::store::vector_generations::{
    GraphVectorGenerationStoreV1, PublishedVectorGenerationV1,
};

use super::journey_test_support::git;
use super::semantic_activation_journey_test::{
    assert_semantic_probe_contribution, evaluate_native_profile, installed_selection_material,
    seed_distribution_fixture, selection, set_semantic_profile, wait_for_semantic_generation,
};
use super::*;

const PROBE_SYMBOL: &str = "semantic_restart_probe";

/// The restarted composition must reload the verified model install and
/// re-seat the retained generation inside this window. The bound is the
/// journey's definition of "not loading indefinitely"; it is not a tunable.
const RESTART_CONVERGENCE: Duration = Duration::from_mins(3);

/// One strict semantic search, decoded whether the lane answered or refused.
///
/// A strict refusal is carried as a tool error with a typed JSON payload; a
/// transport (JSON-RPC) error is the one outcome that means the query was
/// blocked instead of answered, and it fails the journey immediately.
async fn strict_search(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> (bool, Value) {
    let response = harness
        .call_tool(
            project,
            "tracedecay_search",
            json!({
                "query": PROBE_SYMBOL,
                "limit": 10,
                "format": "json",
                "semantic_mode": "strict_semantic",
            }),
        )
        .await
        .expect("strict semantic search answers instead of blocking");
    assert!(
        response.error.is_none(),
        "strict semantic search must answer with a typed payload, not a transport error: {response:?}"
    );
    let result = response.result.as_ref().expect("strict semantic result");
    let refused = result["isError"] == json!(true);
    let text = result["content"][0]["text"]
        .as_str()
        .expect("strict semantic tool text");
    let payload = serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("strict semantic tool did not return JSON: {error}; result={result}; text={text}")
    });
    (refused, payload)
}

/// The public semantic runtime state (`semantic_runtime.state`) of a project.
async fn semantic_runtime_state(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Value {
    let response = harness
        .call_tool(project, "tracedecay_runtime", json!({"format": "json"}))
        .await
        .expect("public production runtime status");
    super::journey_test_support::tool_payload(&response)["semantic_runtime"]["state"].clone()
}

/// Wait until the strict lane completes against a ready runtime, failing
/// closed on a failed runtime and on the convergence bound.
async fn wait_for_strict_semantic_answer(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    checkpoint: &str,
) -> (Value, Value) {
    let mut latest = (Value::Null, Value::Null);
    let converged = tokio::time::timeout(RESTART_CONVERGENCE, async {
        loop {
            let (refused, strict) = strict_search(harness, project).await;
            let state = semantic_runtime_state(harness, project).await;
            assert_ne!(
                state["state"],
                json!("failed"),
                "{checkpoint}: the semantic runtime must not fail: {state}"
            );
            if !refused
                && strict["semantic"]["status"] == json!("complete")
                && state["state"] == json!("ready")
            {
                return (strict, state);
            }
            if refused {
                assert_eq!(
                    strict["status"],
                    json!("unavailable"),
                    "{checkpoint}: a strict refusal must be typed unavailability: {strict}"
                );
                assert!(
                    strict["reason"].is_string(),
                    "{checkpoint}: a strict refusal must name its reason: {strict}"
                );
            }
            latest = (strict, state);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    converged.unwrap_or_else(|_| {
        let (strict, state) = latest;
        panic!(
            "{checkpoint}: strict semantic did not answer within {RESTART_CONVERGENCE:?} \
             (indefinite loading); latest search={strict} runtime={state}"
        )
    })
}

/// The code generation queries pin on a mounted project, once the restarted
/// scheduler serves one.
///
/// A quiet remount restores the sealed generation into the text owner and
/// deliberately never seats the graph-bearing serving slot, so the identity
/// is read from the same seat `latest_generation_id` answers for queries and
/// the sealed generation is loaded from the durable publication it names.
async fn wait_for_serving_code_generation(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1> {
    let schedulers = &harness
        .resources
        .as_ref()
        .expect("live harness")
        .invocation
        .code_index_schedulers;
    tokio::time::timeout(RESTART_CONVERGENCE, async {
        loop {
            if let Some(generation_id) = schedulers.latest_generation_id(project).await
                && let Some(Ok(Some(code))) = schedulers
                    .published_generation(project, &generation_id)
                    .await
            {
                return code;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the restarted code index did not serve a code generation")
}

/// Read the exact retained vector generation the daemon serves for `code`.
async fn retained_vector_generation(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    code: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    vector_id: &tracedecay_domain::VectorGenerationIdV1,
) -> Option<PublishedVectorGenerationV1> {
    let provider = harness
        .resources
        .as_ref()
        .expect("live harness")
        .invocation
        .code_index_schedulers
        .semantic_vector_graph_provider(project)
        .await
        .expect("daemon semantic vector graph provider");
    let retained = provider
        .graph_for_generation(code)
        .await
        .expect("retain the serving semantic vector graph");
    let store = GraphVectorGenerationStoreV1::read_only_generation(&retained, vector_id)
        .expect("read the retained vector generation store")?;
    store
        .generation(vector_id, Arc::clone(retained.cancellation()))
        .await
        .expect("read the retained vector generation catalog")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_semantic_answers_again_after_daemon_restart_without_rebuild() {
    // The journey needs the byte-pinned FastEmbed package from distribution
    // acceptance; it cannot be synthesized, and the ordinary test lane has no
    // reason to have it. Skip explicitly rather than fail the lane, matching
    // the sibling semantic journeys.
    let Some(fixture_root) = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    else {
        eprintln!(
            "skipping the semantic restart journey; prepare the distribution-acceptance \
             package and set TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE"
        );
        return;
    };

    // ---- Fresh install. ---------------------------------------------------
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
        format!("pub fn {PROBE_SYMBOL}() -> &'static str {{ \"restart\" }}\n"),
    )
    .expect("journey source");
    git(&project, &["add", "."]);
    git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "test: seed semantic restart journey",
        ],
    );

    // ---- Activate. --------------------------------------------------------
    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");
    let code_id = harness
        .resources
        .as_ref()
        .expect("live harness")
        .invocation
        .code_index_schedulers
        .latest_generation_id(&project)
        .await
        .expect("published code generation");
    let (code, vector) = wait_for_semantic_generation(&harness, &project, &code_id).await;
    let accepted_profile = evaluate_native_profile(&harness, &project).await;
    set_semantic_profile(
        &harness,
        &project,
        selection(accepted_profile, &artifact_digest, &artifact_path),
        None,
    )
    .await;

    // ---- Strict query. ----------------------------------------------------
    let (activated, activated_state) =
        wait_for_strict_semantic_answer(&harness, &project, "activation").await;
    assert_semantic_probe_contribution(&activated, PROBE_SYMBOL, "strict query after activation");
    assert_eq!(
        activated["code_generation"],
        json!(code.manifest().generation_id),
        "the activated answer must be bound to the serving code generation"
    );
    assert_eq!(
        activated_state["receipt"]["activated_generation"],
        json!(vector.generation_id()),
        "activation must bind the exact evaluated vector generation"
    );
    let evaluated_manifest_digest = vector.manifest_digest().clone();
    let evaluated_content_identity = code.snapshot().content_identity.clone();
    drop(code);

    // ---- Daemon restart. --------------------------------------------------
    harness.shutdown().await;
    let restarted =
        ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
            .await
            .expect("restart the production composition over the same durable state");

    // ---- Query again. -----------------------------------------------------
    let (served, served_state) =
        wait_for_strict_semantic_answer(&restarted, &project, "restart").await;
    assert_semantic_probe_contribution(&served, PROBE_SYMBOL, "strict query after restart");
    assert_eq!(
        served_state["receipt"]["activated_generation"],
        json!(vector.generation_id()),
        "restart must re-seat the evaluated vector generation, not rebuild one: {served_state}"
    );

    // The restarted code index may or may not mint a new generation identifier
    // for the unchanged bytes. Either way the served answer is bound to the
    // generation the scheduler actually serves, that generation seals the same
    // source content the vectors were evaluated from, and the coherence
    // contract admits them explicitly.
    let serving = wait_for_serving_code_generation(&restarted, &project).await;
    assert_eq!(
        served["code_generation"],
        json!(serving.manifest().generation_id),
        "the restarted answer must be bound to the restarted serving generation"
    );
    assert_eq!(
        serving.snapshot().content_identity,
        evaluated_content_identity,
        "an unchanged tree must re-seal the same source content identity"
    );
    let retained = retained_vector_generation(
        &restarted,
        &project,
        &serving,
        vector.generation_id(),
    )
    .await
    .unwrap_or_else(|| {
        panic!(
            "the evaluated vector generation {:?} must survive restart in the retained store (no store wipe)",
            vector.generation_id()
        )
    });
    assert_eq!(
        retained.manifest_digest(),
        &evaluated_manifest_digest,
        "the retained vector generation must be byte-identical to the evaluated one (no rebuild)"
    );
    assert!(
        matches!(
            semantic_source_coherence(&retained, &serving),
            SemanticSourceCoherenceOutcomeV1::Coherent(_)
        ),
        "the retained vectors must be admitted for the restarted serving generation: \
         vector_source={} serving={}",
        retained.source_generation(),
        serving.manifest().generation_id
    );
    if retained.source_generation() != &serving.manifest().generation_id {
        eprintln!(
            "restart republished the unchanged tree as {} (evaluated from {}); semantic stayed valid on the source-content proof",
            serving.manifest().generation_id,
            retained.source_generation()
        );
    }
    restarted.shutdown().await;
}
